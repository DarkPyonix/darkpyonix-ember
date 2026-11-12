//! File operations: stat, ranged read with whole-file hash, atomic write with a hash
//! precondition, directory listing, glob and grep (FR-X1), and the namespace operations a
//! mounted filesystem needs (lstat, readlink, symlink, mkdir, remove, rename, setattr, pwrite;
//! ember server's project mount, `ember/server/src/computers/mount`).
//!
//! All functions are blocking; the API layer runs them on the blocking pool.
//!
//! Glob and grep use ripgrep's own libraries (`ignore`, `globset`, `grep-searcher`,
//! `grep-regex`) in-process rather than shelling out to `rg`: `rg` is not installed on a stock
//! macOS or Raspberry Pi OS, a spawned process per search costs more than the search on small
//! trees, and the libraries give the same ignore rules and regex engine with typed results.
//!
//! **Open, then check (#468).** Content operations (read, pwrite, setattr, grep, and on Unix the
//! atomic write) open the file first and check the opened handle with
//! [`PathPolicy::check_opened`], so a symlink swapped into the path between the policy check and
//! the open cannot make them read or write outside the roots. On Unix the final component is
//! opened with `O_NOFOLLOW`, and the atomic write creates, hashes and renames inside a directory
//! handle (`openat`, `renameat`) that was itself checked. Namespace operations (mkdir, remove,
//! rename, symlink, stat, lstat, readlink, list) still act on paths: a swap of a parent directory
//! between their check and their system call can redirect them. That residual is recorded in
//! SPEC (FR-X1, #468).

use std::fs::{self, File, Metadata};
use std::io::{self, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use globset::{Glob, GlobMatcher};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::Lossy;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use sha2::{Digest, Sha256};

use crate::policy::PathPolicy;
use crate::proto::*;

#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error(transparent)]
    Policy(#[from] crate::policy::PolicyError),
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("precondition failed: expected {expected:?}, found {actual:?}")]
    Precondition { expected: Expect, actual: Option<String> },
    #[error("{0}")]
    BadRequest(String),
}

pub fn mtime_ms(m: &Metadata) -> Option<u64> {
    m.modified().ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis() as u64)
}

fn kind_of(ft: fs::FileType) -> FileKind {
    if ft.is_symlink() {
        FileKind::Symlink
    } else if ft.is_dir() {
        FileKind::Dir
    } else if ft.is_file() {
        FileKind::File
    } else {
        FileKind::Other
    }
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    sha256_reader(&mut File::open(path)?)
}

/// SHA-256 of everything `f` yields from its current position.
pub fn sha256_reader(f: &mut impl Read) -> io::Result<String> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

pub fn sha256_bytes(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

#[cfg(unix)]
fn ms_of(secs: i64, nsecs: i64) -> Option<u64> {
    (secs >= 0).then(|| secs as u64 * 1000 + (nsecs.max(0) as u64) / 1_000_000)
}

#[cfg(unix)]
fn stat_of(path: PathBuf, m: &Metadata) -> Stat {
    Stat {
        kind: kind_of(m.file_type()),
        size: m.len(),
        mtime_ms: mtime_ms(m),
        mode: m.permissions().mode(),
        readonly: m.permissions().readonly(),
        ino: Some(m.ino()),
        nlink: Some(m.nlink()),
        atime_ms: ms_of(m.atime(), m.atime_nsec()),
        ctime_ms: ms_of(m.ctime(), m.ctime_nsec()),
        path,
    }
}

#[cfg(not(unix))]
fn stat_of(path: PathBuf, m: &Metadata) -> Stat {
    let readonly = m.permissions().readonly();
    let mode = if m.is_dir() {
        if readonly { 0o555 } else { 0o755 }
    } else if readonly {
        0o444
    } else {
        0o644
    };
    let atime_ms = m.accessed().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64);
    let ctime_ms = m.created().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64);
    Stat {
        kind: kind_of(m.file_type()),
        size: m.len(),
        mtime_ms: mtime_ms(m),
        mode,
        readonly,
        ino: None,
        nlink: None,
        atime_ms,
        ctime_ms,
        path,
    }
}

/// The portable name of an OS error number, for [`ErrorBody::errno`].
pub fn errno_name(raw: i32) -> Option<&'static str> {
    Some(match raw {
        libc::EPERM => "EPERM",
        libc::ENOENT => "ENOENT",
        libc::EIO => "EIO",
        libc::EACCES => "EACCES",
        libc::EBUSY => "EBUSY",
        libc::EEXIST => "EEXIST",
        libc::EXDEV => "EXDEV",
        libc::ENOTDIR => "ENOTDIR",
        libc::EISDIR => "EISDIR",
        libc::EINVAL => "EINVAL",
        #[cfg(unix)]
        libc::ETXTBSY => "ETXTBSY",
        libc::EFBIG => "EFBIG",
        libc::ENOSPC => "ENOSPC",
        libc::EROFS => "EROFS",
        libc::EMLINK => "EMLINK",
        libc::ENAMETOOLONG => "ENAMETOOLONG",
        libc::ENOTEMPTY => "ENOTEMPTY",
        #[cfg(unix)]
        libc::ELOOP => "ELOOP",
        #[cfg(unix)]
        libc::EDQUOT => "EDQUOT",
        #[cfg(unix)]
        libc::ENOTSUP => "ENOTSUP",
        _ => return None,
    })
}

/// [`errno_name`] for an I/O error, falling back to its kind when it carries no OS error.
pub fn io_errno(e: &io::Error) -> Option<&'static str> {
    if let Some(n) = e.raw_os_error().and_then(errno_name) {
        return Some(n);
    }
    Some(match e.kind() {
        io::ErrorKind::NotFound => "ENOENT",
        io::ErrorKind::PermissionDenied => "EACCES",
        io::ErrorKind::AlreadyExists => "EEXIST",
        io::ErrorKind::InvalidInput => "EINVAL",
        _ => return None,
    })
}

impl FsError {
    /// The portable errno name of this error, if it has one.
    pub fn errno(&self) -> Option<&'static str> {
        match self {
            FsError::Policy(crate::policy::PolicyError::Io(_, e)) | FsError::Io(e) => io_errno(e),
            FsError::Policy(crate::policy::PolicyError::Outside(_)) => Some("EACCES"),
            FsError::Policy(crate::policy::PolicyError::NotAbsolute(_)) => Some("EINVAL"),
            FsError::Precondition { .. } => None,
            FsError::BadRequest(_) => Some("EINVAL"),
        }
    }
}

/// An OS error by number (so [`FsError::errno`] is exact); the context goes to the log, since
/// `io::Error` cannot carry both an OS code and a custom message.
fn io_err(raw: i32, context: String) -> FsError {
    tracing::debug!("{context}");
    FsError::Io(io::Error::from_raw_os_error(raw))
}

/// Resolve `path` (which must exist), open it with `opts` (on Unix also `O_NOFOLLOW` and
/// `extra_flags`) and check the opened handle (#468). Returns the handle and its canonical path.
fn open_checked(
    policy: &PathPolicy,
    path: &Path,
    opts: &mut fs::OpenOptions,
    #[allow(unused_variables)] extra_flags: i32,
) -> Result<(File, PathBuf), FsError> {
    let p = policy.resolve_existing(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | extra_flags);
    }
    let f = opts.open(&p)?;
    let p = policy.check_opened(&f, &p)?;
    Ok((f, p))
}

/// Directory-relative system calls for the race-free atomic write (#468). Every name is opened
/// with `O_NOFOLLOW`, so a symlink swapped in at the name is refused, not followed.
#[cfg(unix)]
mod at {
    use std::ffi::{CString, OsStr};
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    fn cstr(b: &[u8]) -> io::Result<CString> {
        CString::new(b).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
    }

    /// Open a directory without following a final symlink.
    pub fn open_dir(path: &Path) -> io::Result<File> {
        let c = cstr(path.as_os_str().as_bytes())?;
        // SAFETY: `c` is a valid NUL-terminated path; the flags take no extra argument.
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor this function owns.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    /// `openat(dir, name, flags | O_NOFOLLOW | O_CLOEXEC, mode)`.
    pub fn openat(dir: &File, name: &OsStr, flags: libc::c_int, mode: libc::mode_t) -> io::Result<File> {
        let c = cstr(name.as_bytes())?;
        // SAFETY: `dir` is an open descriptor, `c` a valid NUL-terminated name, `mode` is read
        // only with O_CREAT and is passed as the promoted unsigned int openat(2) expects.
        let fd = unsafe {
            libc::openat(dir.as_raw_fd(), c.as_ptr(), flags | libc::O_NOFOLLOW | libc::O_CLOEXEC, mode as libc::c_uint)
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor this function owns.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    /// `fstatat(dir, name, AT_SYMLINK_NOFOLLOW)`: the entry's `st_mode`.
    pub fn lstat_mode(dir: &File, name: &OsStr) -> io::Result<u32> {
        let c = cstr(name.as_bytes())?;
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `st` is writable storage for one `stat`, filled on success.
        let rc = unsafe { libc::fstatat(dir.as_raw_fd(), c.as_ptr(), st.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fstatat succeeded, so `st` is initialised.
        Ok(unsafe { st.assume_init() }.st_mode as u32)
    }

    pub fn renameat(dir: &File, from: &OsStr, to: &OsStr) -> io::Result<()> {
        let (f, t) = (cstr(from.as_bytes())?, cstr(to.as_bytes())?);
        // SAFETY: both names are valid NUL-terminated strings relative to the open `dir`.
        let rc = unsafe { libc::renameat(dir.as_raw_fd(), f.as_ptr(), dir.as_raw_fd(), t.as_ptr()) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn unlinkat(dir: &File, name: &OsStr) -> io::Result<()> {
        let c = cstr(name.as_bytes())?;
        // SAFETY: `c` is a valid NUL-terminated name relative to the open `dir`.
        let rc = unsafe { libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), 0) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

pub fn stat(policy: &PathPolicy, path: &Path) -> Result<Stat, FsError> {
    let p = policy.resolve_existing(path)?;
    let m = fs::metadata(&p)?;
    Ok(stat_of(p, &m))
}

/// Like [`stat`] but does not follow a final symbolic link.
pub fn lstat(policy: &PathPolicy, path: &Path) -> Result<Stat, FsError> {
    let p = policy.resolve_nofollow(path)?;
    let m = fs::symlink_metadata(&p)?;
    Ok(stat_of(p, &m))
}

pub fn readlink(policy: &PathPolicy, path: &Path) -> Result<ReadlinkResponse, FsError> {
    let p = policy.resolve_nofollow(path)?;
    let target = fs::read_link(&p)?;
    Ok(ReadlinkResponse { path: p, target })
}

pub fn symlink(policy: &PathPolicy, req: &SymlinkRequest) -> Result<Stat, FsError> {
    let p = policy.resolve_nofollow(&req.path)?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&req.target, &p)?;
    #[cfg(windows)]
    {
        let is_dir = req.target.is_dir()
            || p.parent().map(|d| d.join(&req.target).is_dir()).unwrap_or(false);
        if is_dir {
            std::os::windows::fs::symlink_dir(&req.target, &p)?;
        } else {
            std::os::windows::fs::symlink_file(&req.target, &p)?;
        }
    }
    let m = fs::symlink_metadata(&p)?;
    Ok(stat_of(p, &m))
}

pub fn mkdir(policy: &PathPolicy, req: &MkdirRequest) -> Result<Stat, FsError> {
    let p = if req.parents { policy.resolve_for_write(&req.path)? } else { policy.resolve_nofollow(&req.path)? };
    let mut b = fs::DirBuilder::new();
    b.recursive(req.parents);
    #[cfg(unix)]
    if let Some(mode) = req.mode {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(mode & 0o7777);
    }
    b.create(&p)?;
    if req.parents {
        // Re-check after creating: a symlink planted along the way must not take us outside.
        policy.resolve_existing(&p)?;
    }
    let m = fs::symlink_metadata(&p)?;
    Ok(stat_of(p, &m))
}

pub fn remove(policy: &PathPolicy, req: &RemoveRequest) -> Result<(), FsError> {
    let p = policy.resolve_nofollow(&req.path)?;
    if policy.is_root(&p) {
        return Err(io_err(libc::EBUSY, format!("{} is an allowed root", p.display())));
    }
    let m = fs::symlink_metadata(&p)?;
    if m.is_dir() {
        if req.recursive {
            // `remove_dir_all` does not follow symlinks inside the tree.
            fs::remove_dir_all(&p)?;
        } else {
            fs::remove_dir(&p)?;
        }
    } else {
        fs::remove_file(&p)?;
    }
    Ok(())
}

pub fn rename(policy: &PathPolicy, req: &RenameRequest) -> Result<(), FsError> {
    let from = policy.resolve_nofollow(&req.from)?;
    let to = policy.resolve_nofollow(&req.to)?;
    if policy.is_root(&from) || policy.is_root(&to) {
        return Err(io_err(libc::EBUSY, "an allowed root cannot be renamed or replaced".into()));
    }
    fs::symlink_metadata(&from)?;
    if !req.overwrite && fs::symlink_metadata(&to).is_ok() {
        return Err(io_err(libc::EEXIST, format!("{} exists", to.display())));
    }
    fs::rename(&from, &to)?;
    Ok(())
}

#[cfg(unix)]
fn timespec(t: Option<SetTime>) -> libc::timespec {
    match t {
        None => libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_OMIT as _ },
        Some(SetTime::Now) => libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_NOW as _ },
        Some(SetTime::UnixMs(ms)) => libc::timespec {
            tv_sec: (ms / 1000) as libc::time_t,
            tv_nsec: ((ms % 1000) * 1_000_000) as _,
        },
    }
}

/// Apply size, mode and times (see [`SetAttrRequest`]). Callers serialise this with writes.
///
/// The size is set through a checked handle. On Unix the mode and times are too (`fchmod`,
/// `futimens`); a file this user cannot open for reading (mode `0o000`) falls back to the path.
pub fn setattr(policy: &PathPolicy, req: &SetAttrRequest) -> Result<Stat, FsError> {
    let p = policy.resolve_existing(&req.path)?;
    if let Some(size) = req.size {
        let m = fs::metadata(&p)?;
        if m.is_dir() {
            return Err(io_err(libc::EISDIR, format!("{} is a directory", p.display())));
        }
        let (f, _) = open_checked(policy, &p, fs::OpenOptions::new().write(true), 0)?;
        f.set_len(size)?;
    }
    let times = req.atime.is_some() || req.mtime.is_some();
    if req.mode.is_some() || times {
        #[cfg(unix)]
        match open_checked(policy, &p, fs::OpenOptions::new().read(true), libc::O_NONBLOCK) {
            Ok((f, _)) => setattr_fd(&f, req)?,
            Err(FsError::Io(e)) if e.kind() == io::ErrorKind::PermissionDenied => setattr_path(&p, req)?,
            Err(e) => return Err(e),
        }
        #[cfg(not(unix))]
        setattr_path(&p, req)?;
    }
    let p = policy.resolve_existing(&p)?;
    let m = fs::metadata(&p)?;
    Ok(stat_of(p, &m))
}

#[cfg(unix)]
fn setattr_fd(f: &File, req: &SetAttrRequest) -> Result<(), FsError> {
    use std::os::fd::AsRawFd;
    if let Some(mode) = req.mode {
        f.set_permissions(fs::Permissions::from_mode(mode & 0o7777))?;
    }
    if req.atime.is_some() || req.mtime.is_some() {
        let times = [timespec(req.atime), timespec(req.mtime)];
        // SAFETY: `f` is an open descriptor and `times` has the two entries futimens(2) reads.
        let rc = unsafe { libc::futimens(f.as_raw_fd(), times.as_ptr()) };
        if rc != 0 {
            return Err(io::Error::last_os_error().into());
        }
    }
    Ok(())
}

/// Mode and times by path (Windows, and a Unix file that cannot be opened for reading).
fn setattr_path(p: &Path, req: &SetAttrRequest) -> Result<(), FsError> {
    if let Some(mode) = req.mode {
        #[cfg(unix)]
        fs::set_permissions(p, fs::Permissions::from_mode(mode & 0o7777))?;
        #[cfg(not(unix))]
        {
            let mut perms = fs::metadata(p)?.permissions();
            perms.set_readonly(mode & 0o200 == 0);
            fs::set_permissions(p, perms)?;
        }
    }
    #[cfg(unix)]
    if req.atime.is_some() || req.mtime.is_some() {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(p.as_os_str().as_bytes())
            .map_err(|_| FsError::BadRequest("path contains NUL".into()))?;
        let times = [timespec(req.atime), timespec(req.mtime)];
        // SAFETY: `c` is a valid NUL-terminated path and `times` has the two entries
        // utimensat(2) reads.
        let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) };
        if rc != 0 {
            return Err(io::Error::last_os_error().into());
        }
    }
    #[cfg(not(unix))]
    if req.atime.is_some() || req.mtime.is_some() {
        let to_sys = |t: SetTime| match t {
            SetTime::Now => std::time::SystemTime::now(),
            SetTime::UnixMs(ms) => UNIX_EPOCH + std::time::Duration::from_millis(ms),
        };
        let mut times = fs::FileTimes::new();
        if let Some(a) = req.atime {
            times = times.set_accessed(to_sys(a));
        }
        if let Some(m) = req.mtime {
            times = times.set_modified(to_sys(m));
        }
        fs::OpenOptions::new().write(true).open(p)?.set_times(times)?;
    }
    Ok(())
}

/// Positional in-place write into an existing regular file (see [`PwriteRequest`]).
pub fn pwrite(policy: &PathPolicy, req: &PwriteRequest) -> Result<Stat, FsError> {
    #[allow(unused_mut)]
    let (mut f, p) = open_checked(policy, &req.path, fs::OpenOptions::new().write(true), 0)?;
    let m = f.metadata()?;
    if !m.is_file() {
        return Err(io_err(libc::EINVAL, format!("{} is not a regular file", p.display())));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        f.write_all_at(&req.data, req.offset)?;
    }
    #[cfg(not(unix))]
    {
        f.seek(SeekFrom::Start(req.offset))?;
        f.write_all(&req.data)?;
    }
    let m = f.metadata()?;
    Ok(stat_of(p, &m))
}

pub fn read(policy: &PathPolicy, req: &ReadRequest) -> Result<ReadResponse, FsError> {
    let (mut f, p) = open_checked(policy, &req.path, fs::OpenOptions::new().read(true), 0)?;
    let m = f.metadata()?;
    if m.is_dir() {
        return Err(FsError::BadRequest(format!("{} is a directory", p.display())));
    }
    let size = m.len();
    let len = req.len.unwrap_or(MAX_READ).min(MAX_READ);
    let mut data = Vec::new();
    if req.offset < size {
        f.seek(SeekFrom::Start(req.offset))?;
        (&mut f).take(len).read_to_end(&mut data)?;
    }
    // Hash the opened file, not the path again (#468).
    let sha256 = if req.hash {
        f.seek(SeekFrom::Start(0))?;
        Some(sha256_reader(&mut f)?)
    } else {
        None
    };
    Ok(ReadResponse {
        eof: req.offset + data.len() as u64 >= size,
        path: p,
        size,
        mtime_ms: mtime_ms(&m),
        sha256,
        offset: req.offset,
        data,
    })
}

/// Atomic write: the data goes to a temporary file in the target's directory, is flushed to disk
/// and renamed over the target, so readers see the old or the new content, never a mix. An
/// existing file's permission bits are kept. The precondition is checked immediately before the
/// rename; callers serialise writes (the API holds one write lock) so the check-then-rename is
/// atomic against other writes through this daemon, though not against other processes.
///
/// On Unix the target directory is opened (`O_NOFOLLOW`) and checked once, and the existing
/// file's mode, the hash precondition, the temporary file and the rename all go through that
/// handle (#468), so a symlink swapped into the path cannot move the write outside the roots.
#[cfg(unix)]
pub fn write(policy: &PathPolicy, req: &WriteRequest) -> Result<WriteResponse, FsError> {
    let p = policy.resolve_for_write(&req.path)?;
    let dir = p.parent().ok_or_else(|| FsError::BadRequest("no parent directory".into()))?;
    let name = p.file_name().ok_or_else(|| FsError::BadRequest("no file name".into()))?.to_os_string();
    if req.create_parents {
        fs::create_dir_all(dir)?;
    }
    let d = at::open_dir(dir)?;
    policy.check_opened(&d, dir)?;

    let existing_mode = match at::lstat_mode(&d, &name) {
        Ok(mode) => match mode & libc::S_IFMT as u32 {
            m if m == libc::S_IFDIR as u32 => {
                return Err(FsError::BadRequest(format!("{} is a directory", p.display())))
            }
            m if m == libc::S_IFLNK as u32 => {
                return Err(io_err(libc::ELOOP, format!("{} became a symbolic link", p.display())))
            }
            _ => Some(mode & 0o7777),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };

    let tmp = std::ffi::OsString::from(format!(
        ".{}.ember-{}.tmp",
        name.to_string_lossy(),
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| -> Result<(), FsError> {
        let mut f = at::openat(&d, &tmp, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o666)?;
        f.write_all(&req.data)?;
        if let Some(mode) = existing_mode {
            f.set_permissions(fs::Permissions::from_mode(mode))?;
        }
        f.sync_all()?;
        drop(f);

        if let Some(expected) = &req.expect {
            let actual = match existing_mode {
                Some(_) => Some(sha256_reader(&mut at::openat(&d, &name, libc::O_RDONLY | libc::O_NONBLOCK, 0)?)?),
                None => None,
            };
            let ok = match expected {
                Expect::Absent => actual.is_none(),
                Expect::Sha256(h) => actual.as_deref() == Some(h.to_ascii_lowercase().as_str()),
            };
            if !ok {
                return Err(FsError::Precondition { expected: expected.clone(), actual });
            }
        }
        at::renameat(&d, &tmp, &name)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = at::unlinkat(&d, &tmp);
    }
    result?;
    // Persist the rename itself.
    let _ = d.sync_all();
    Ok(WriteResponse { size: req.data.len() as u64, sha256: sha256_bytes(&req.data), path: p })
}

/// The path-based atomic write (Windows); see the Unix variant for the checked one.
#[cfg(not(unix))]
pub fn write(policy: &PathPolicy, req: &WriteRequest) -> Result<WriteResponse, FsError> {
    let p = policy.resolve_for_write(&req.path)?;
    let dir = p.parent().ok_or_else(|| FsError::BadRequest("no parent directory".into()))?;
    if req.create_parents {
        fs::create_dir_all(dir)?;
        // Re-check: created directories must still be inside a root (no race-free guarantee,
        // but catches a symlink planted along the way).
        policy.resolve_existing(dir)?;
    }

    let existing = match fs::metadata(&p) {
        Ok(m) if m.is_dir() => {
            return Err(FsError::BadRequest(format!("{} is a directory", p.display())))
        }
        Ok(m) => Some(m),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };

    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.ember-{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| -> Result<(), FsError> {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(&req.data)?;
        if let Some(m) = &existing {
            #[cfg(unix)]
            f.set_permissions(fs::Permissions::from_mode(m.permissions().mode()))?;
            #[cfg(not(unix))]
            let _ = f.set_permissions(m.permissions());
        }
        f.sync_all()?;
        drop(f);

        if let Some(expected) = &req.expect {
            let actual = match &existing {
                Some(_) => Some(sha256_file(&p)?),
                None => None,
            };
            let ok = match expected {
                Expect::Absent => actual.is_none(),
                Expect::Sha256(h) => actual.as_deref() == Some(h.to_ascii_lowercase().as_str()),
            };
            if !ok {
                return Err(FsError::Precondition { expected: expected.clone(), actual });
            }
        }
        fs::rename(&tmp, &p)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    // Persist the rename itself.
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(WriteResponse { size: req.data.len() as u64, sha256: sha256_bytes(&req.data), path: p })
}

pub fn list(policy: &PathPolicy, path: &Path) -> Result<ListResponse, FsError> {
    let p = policy.resolve_existing(path)?;
    let mut entries = Vec::new();
    for e in fs::read_dir(&p)? {
        let e = e?;
        // A denied entry is not listed, so the listing does not reveal the exclusion (#534).
        if policy.is_denied(&p.join(e.file_name())) {
            continue;
        }
        let Ok(m) = e.metadata() else { continue };
        entries.push(DirEntry {
            name: e.file_name().to_string_lossy().into_owned(),
            kind: kind_of(m.file_type()),
            size: m.len(),
            mtime_ms: mtime_ms(&m),
            #[cfg(unix)]
            mode: Some(m.permissions().mode()),
            #[cfg(not(unix))]
            mode: None,
            #[cfg(unix)]
            ino: Some(m.ino()),
            #[cfg(not(unix))]
            ino: None,
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(ListResponse { path: p, entries })
}

/// `O_NONBLOCK` on Unix (so opening a FIFO does not hang), nothing elsewhere.
fn nonblock() -> i32 {
    #[cfg(unix)]
    return libc::O_NONBLOCK;
    #[cfg(not(unix))]
    0
}

fn matcher(pattern: &str) -> Result<GlobMatcher, FsError> {
    Glob::new(pattern)
        .map(|g| g.compile_matcher())
        .map_err(|e| FsError::BadRequest(format!("bad glob {pattern:?}: {e}")))
}

fn literal_sep_matcher(pattern: &str) -> Result<GlobMatcher, FsError> {
    globset::GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|e| FsError::BadRequest(format!("bad glob {pattern:?}: {e}")))
}

/// Files under `root`, honouring ignore files when asked. Never follows symlinks, so the walk
/// stays inside the (already checked) root.
///
/// A denied directory (see [`PathPolicy::with_denied`]) is not descended into, so a search over
/// a root never reads inside it.
fn walk(policy: &PathPolicy, root: &Path, respect_ignore: bool) -> impl Iterator<Item = ignore::DirEntry> {
    let denied: Vec<PathBuf> = policy.denied().to_vec();
    ignore::WalkBuilder::new(root)
        .standard_filters(respect_ignore)
        .filter_entry(move |e| !denied.iter().any(|d| e.path().starts_with(d)))
        .require_git(false)
        .follow_links(false)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
}

pub fn glob(policy: &PathPolicy, req: &GlobRequest) -> Result<GlobResponse, FsError> {
    let root = policy.resolve_existing(&req.root)?;
    let m = literal_sep_matcher(&req.pattern)?;
    let limit = req.limit.unwrap_or(1000);
    let mut found: Vec<(Option<u64>, PathBuf)> = Vec::new();
    for e in walk(policy, &root, req.respect_ignore) {
        let rel = e.path().strip_prefix(&root).unwrap_or(e.path());
        if m.is_match(rel) {
            let mt = e.metadata().ok().as_ref().and_then(mtime_ms);
            found.push((mt, e.into_path()));
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let truncated = found.len() > limit;
    found.truncate(limit);
    Ok(GlobResponse { paths: found.into_iter().map(|(_, p)| p).collect(), truncated })
}

pub fn grep(policy: &PathPolicy, req: &GrepRequest) -> Result<GrepResponse, FsError> {
    let root = policy.resolve_existing(&req.root)?;
    let re = RegexMatcherBuilder::new()
        .case_insensitive(req.case_insensitive)
        .build(&req.pattern)
        .map_err(|e| FsError::BadRequest(format!("bad pattern: {e}")))?;
    let filter = req.glob.as_deref().map(matcher).transpose()?;
    let limit = req.limit.unwrap_or(1000);
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(0))
        .build();

    let mut matches = Vec::new();
    let mut truncated = false;
    let files: Box<dyn Iterator<Item = PathBuf>> = if root.is_file() {
        Box::new(std::iter::once(root.clone()))
    } else {
        Box::new(walk(policy, &root, req.respect_ignore).map(|e| e.into_path()))
    };
    'files: for path in files {
        if let Some(f) = &filter {
            let rel = path.strip_prefix(&root).unwrap_or(&path);
            // Match the relative path or the bare file name, as `rg --glob '*.rs'` does.
            if !f.is_match(rel) && !path.file_name().is_some_and(|n| f.is_match(n)) {
                continue;
            }
        }
        // Open, then check the handle (#468): a swapped symlink cannot make grep read outside.
        let file = match open_checked(policy, &path, fs::OpenOptions::new().read(true), nonblock()) {
            Ok((f, _)) => f,
            Err(e) => {
                tracing::debug!(path = %path.display(), "grep skipped: {e}");
                continue;
            }
        };
        let mut hit_limit = false;
        let res = searcher.search_file(
            &re,
            &file,
            Lossy(|line, text| {
                if matches.len() >= limit {
                    hit_limit = true;
                    return Ok(false);
                }
                let mut t = text.trim_end_matches(['\n', '\r']).to_string();
                if t.len() > 2000 {
                    let mut cut = 2000;
                    while !t.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    t.truncate(cut);
                }
                matches.push(GrepMatch { path: path.clone(), line, text: t });
                Ok(true)
            }),
        );
        if let Err(e) = res {
            tracing::debug!(path = %path.display(), "grep skipped: {e}");
        }
        if hit_limit {
            truncated = true;
            break 'files;
        }
    }
    Ok(GrepResponse { matches, truncated })
}

