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
//! handle (`openat`, `renameat`) that was itself checked.
//!
//! **Namespace operations (#456, INTENT D48).** On Unix, mkdir, remove, rename, symlink, stat,
//! lstat and readlink open the PARENT directory with `O_DIRECTORY | O_NOFOLLOW`, check that
//! handle the same way (resolve again, same device and inode), and act on the final name
//! relative to it (`mkdirat`, `unlinkat`, `renameat` between two checked parents, `symlinkat`,
//! `fstatat`, `readlinkat`). `mkdir -p` creates one level at a time through checked parents, and
//! a recursive remove walks the tree through `O_NOFOLLOW` directory handles. `list` opens and
//! checks the directory itself and reads it from the handle. A parent swapped for a symlink
//! after the policy check is therefore refused, not followed. `glob` still walks by path; every
//! match is read again with the checked `lstat`, so only a name that also exists at the same
//! place inside can pass. `stat` and `lstat` of a root read it by path (its parent is outside
//! the roots; roots are fixed by configuration). `symlink` also checks the link's target: a
//! target that resolves outside the roots or into a denied entry is refused.
//!
//! On Windows the namespace operations stay path based: std has no directory-relative calls
//! and no stable file id, so each one resolves the parent again right before its call and
//! refuses when it moved. A swap inside that window is not detected there (recorded in SPEC
//! FR-X1).

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

    /// `fstatat(dir, name, AT_SYMLINK_NOFOLLOW)`: the entry itself, a symbolic link not followed.
    pub fn lstat(dir: &File, name: &OsStr) -> io::Result<libc::stat> {
        let c = cstr(name.as_bytes())?;
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `st` is writable storage for one `stat`, filled on success.
        let rc = unsafe { libc::fstatat(dir.as_raw_fd(), c.as_ptr(), st.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fstatat succeeded, so `st` is initialised.
        Ok(unsafe { st.assume_init() })
    }

    /// `renameat(from_dir, from, to_dir, to)`.
    pub fn renameat(from_dir: &File, from: &OsStr, to_dir: &File, to: &OsStr) -> io::Result<()> {
        let (f, t) = (cstr(from.as_bytes())?, cstr(to.as_bytes())?);
        // SAFETY: both names are valid NUL-terminated strings relative to the open directories.
        let rc = unsafe { libc::renameat(from_dir.as_raw_fd(), f.as_ptr(), to_dir.as_raw_fd(), t.as_ptr()) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// `unlinkat(dir, name, flags)`; `flags` is 0 or `AT_REMOVEDIR`.
    pub fn unlinkat(dir: &File, name: &OsStr, flags: libc::c_int) -> io::Result<()> {
        let c = cstr(name.as_bytes())?;
        // SAFETY: `c` is a valid NUL-terminated name relative to the open `dir`.
        let rc = unsafe { libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), flags) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// `mkdirat(dir, name, mode)`.
    pub fn mkdirat(dir: &File, name: &OsStr, mode: libc::mode_t) -> io::Result<()> {
        let c = cstr(name.as_bytes())?;
        // SAFETY: `c` is a valid NUL-terminated name relative to the open `dir`.
        let rc = unsafe { libc::mkdirat(dir.as_raw_fd(), c.as_ptr(), mode) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// `symlinkat(target, dir, name)`: a link `name` in `dir` holding `target` as given.
    pub fn symlinkat(target: &Path, dir: &File, name: &OsStr) -> io::Result<()> {
        let (t, c) = (cstr(target.as_os_str().as_bytes())?, cstr(name.as_bytes())?);
        // SAFETY: both are valid NUL-terminated strings; `dir` is an open descriptor.
        let rc = unsafe { libc::symlinkat(t.as_ptr(), dir.as_raw_fd(), c.as_ptr()) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// `readlinkat(dir, name)`: the link's target as stored.
    pub fn readlinkat(dir: &File, name: &OsStr) -> io::Result<std::path::PathBuf> {
        use std::os::unix::ffi::OsStringExt;
        let c = cstr(name.as_bytes())?;
        let mut buf: Vec<u8> = vec![0; 4096];
        loop {
            // SAFETY: `buf` is writable for `buf.len()` bytes; readlinkat writes at most that.
            let n = unsafe { libc::readlinkat(dir.as_raw_fd(), c.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            let n = n as usize;
            if n < buf.len() {
                buf.truncate(n);
                return Ok(std::ffi::OsString::from_vec(buf).into());
            }
            // The target may be longer than the buffer: try again with more room.
            let len = buf.len() * 2;
            buf.resize(len, 0);
        }
    }

    /// The names in an open directory, without `.` and `..`.
    pub fn read_names(dir: &File) -> io::Result<Vec<std::ffi::OsString>> {
        // fdopendir takes the descriptor it is given, so give it a duplicate.
        // SAFETY: `dir` is an open descriptor; dup returns a new one or -1.
        let fd = unsafe { libc::dup(dir.as_raw_fd()) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor; on success the stream owns it.
        let dp = unsafe { libc::fdopendir(fd) };
        if dp.is_null() {
            let e = io::Error::last_os_error();
            // SAFETY: fdopendir failed, so `fd` is still ours to close.
            unsafe { libc::close(fd) };
            return Err(e);
        }
        // SAFETY: `dp` is an open directory stream. The duplicate shares the offset, so start
        // from the beginning.
        unsafe { libc::rewinddir(dp) };
        let mut out = Vec::new();
        loop {
            // SAFETY: `dp` is open; readdir returns an entry valid until the next call, or NULL
            // at the end.
            let ent = unsafe { libc::readdir(dp) };
            if ent.is_null() {
                break;
            }
            // SAFETY: `d_name` of a returned entry is a NUL-terminated name.
            let name = unsafe { std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                out.push(OsStr::from_bytes(name).to_os_string());
            }
        }
        // SAFETY: `dp` is open and not used after this.
        unsafe { libc::closedir(dp) };
        Ok(out)
    }

    /// Remove the directory `name` in `dir` and everything inside it, every step relative to a
    /// directory handle opened with `O_NOFOLLOW`, so a symlink swapped into the tree is removed
    /// as a link, never followed.
    pub fn remove_tree(dir: &File, name: &OsStr) -> io::Result<()> {
        let sub = openat(dir, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
        for n in read_names(&sub)? {
            let st = match lstat(&sub, &n) {
                Ok(st) => st,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let r = if st.st_mode as u32 & libc::S_IFMT as u32 == libc::S_IFDIR as u32 {
                remove_tree(&sub, &n)
            } else {
                unlinkat(&sub, &n, 0)
            };
            match r {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        unlinkat(dir, name, libc::AT_REMOVEDIR)
    }
}

/// [`Stat`] from a raw `stat` (an entry read with `fstatat`, Unix).
#[cfg(unix)]
fn stat_raw(path: PathBuf, st: &libc::stat) -> Stat {
    let mode = st.st_mode as u32;
    let fmt = mode & libc::S_IFMT as u32;
    let kind = if fmt == libc::S_IFLNK as u32 {
        FileKind::Symlink
    } else if fmt == libc::S_IFDIR as u32 {
        FileKind::Dir
    } else if fmt == libc::S_IFREG as u32 {
        FileKind::File
    } else {
        FileKind::Other
    };
    Stat {
        kind,
        size: st.st_size as u64,
        mtime_ms: ms_of(st.st_mtime as i64, st.st_mtime_nsec as i64),
        mode,
        readonly: mode & 0o222 == 0,
        ino: Some(st.st_ino as u64),
        nlink: Some(st.st_nlink as u64),
        atime_ms: ms_of(st.st_atime as i64, st.st_atime_nsec as i64),
        ctime_ms: ms_of(st.st_ctime as i64, st.st_ctime_nsec as i64),
        path,
    }
}

/// A checked handle on the parent directory of a namespace operation's entry (#456, D48).
#[cfg(unix)]
struct Parent {
    dir: File,
    /// The entry's canonical path (checked parent plus `name`).
    path: PathBuf,
    name: std::ffi::OsString,
}

/// Open the parent of the canonical entry path `p` with `O_DIRECTORY | O_NOFOLLOW` and check the
/// handle against the policy (resolve again, same device and inode). The operation then acts on
/// `name` relative to that handle, so a parent directory swapped for a symlink after the policy
/// check is refused, not followed (#456).
#[cfg(unix)]
fn open_parent(policy: &PathPolicy, p: PathBuf) -> Result<Parent, FsError> {
    let dir = p.parent().ok_or_else(|| FsError::BadRequest("no parent directory".into()))?;
    let name = p.file_name().ok_or_else(|| FsError::BadRequest("no file name".into()))?.to_os_string();
    let d = at::open_dir(dir)?;
    policy.check_opened(&d, dir)?;
    Ok(Parent { dir: d, path: p, name })
}

/// Windows has no directory-relative calls in std: resolve the parent again right before the
/// path-based call and refuse when it moved (#456). A swap inside that window is not detected.
#[cfg(not(unix))]
fn recheck_parent(policy: &PathPolicy, p: &Path) -> Result<(), FsError> {
    if let Some(dir) = p.parent() {
        if policy.resolve_existing(dir)? != dir {
            return Err(crate::policy::PolicyError::Outside(p.to_path_buf()).into());
        }
    }
    Ok(())
}

/// Follows a final symbolic link (the path is resolved first). On Unix the entry is read with
/// `fstatat` relative to its checked parent (#456); a root, whose parent is outside the roots, is
/// read by its path (roots are fixed by configuration).
pub fn stat(policy: &PathPolicy, path: &Path) -> Result<Stat, FsError> {
    let p = policy.resolve_existing(path)?;
    #[cfg(unix)]
    if !policy.is_root(&p) {
        let par = open_parent(policy, p)?;
        let st = at::lstat(&par.dir, &par.name)?;
        // `p` is canonical, so its final component was no link when it was resolved. A link
        // there now was swapped in: refuse it rather than follow it.
        if st.st_mode as u32 & libc::S_IFMT as u32 == libc::S_IFLNK as u32 {
            return Err(crate::policy::PolicyError::Outside(path.to_path_buf()).into());
        }
        return Ok(stat_raw(par.path, &st));
    }
    let m = fs::metadata(&p)?;
    Ok(stat_of(p, &m))
}

/// Like [`stat`] but does not follow a final symbolic link.
pub fn lstat(policy: &PathPolicy, path: &Path) -> Result<Stat, FsError> {
    let p = policy.resolve_nofollow(path)?;
    #[cfg(unix)]
    if !policy.is_root(&p) {
        let par = open_parent(policy, p)?;
        let st = at::lstat(&par.dir, &par.name)?;
        return Ok(stat_raw(par.path, &st));
    }
    let m = fs::symlink_metadata(&p)?;
    Ok(stat_of(p, &m))
}

pub fn readlink(policy: &PathPolicy, path: &Path) -> Result<ReadlinkResponse, FsError> {
    let p = policy.resolve_nofollow(path)?;
    #[cfg(unix)]
    {
        let par = open_parent(policy, p)?;
        let target = at::readlinkat(&par.dir, &par.name)?;
        Ok(ReadlinkResponse { path: par.path, target })
    }
    #[cfg(not(unix))]
    {
        recheck_parent(policy, &p)?;
        let target = fs::read_link(&p)?;
        Ok(ReadlinkResponse { path: p, target })
    }
}

/// Create a symbolic link at `req.path` holding `req.target` as given. The target must resolve
/// inside the roots too (relative to the link's directory when relative), so the API cannot
/// plant a link that points out (#456). On Unix the link is made with `symlinkat` in the checked
/// parent directory.
pub fn symlink(policy: &PathPolicy, req: &SymlinkRequest) -> Result<Stat, FsError> {
    if req.target.as_os_str().is_empty() {
        return Err(FsError::BadRequest("empty symlink target".into()));
    }
    let p = policy.resolve_nofollow(&req.path)?;
    let dir = p.parent().ok_or_else(|| FsError::BadRequest("no parent directory".into()))?;
    policy.check_link_target(dir, &req.target)?;
    #[cfg(unix)]
    {
        let par = open_parent(policy, p)?;
        at::symlinkat(&req.target, &par.dir, &par.name)?;
        let st = at::lstat(&par.dir, &par.name)?;
        Ok(stat_raw(par.path, &st))
    }
    #[cfg(windows)]
    {
        recheck_parent(policy, &p)?;
        let is_dir = req.target.is_dir() || dir.join(&req.target).is_dir();
        if is_dir {
            std::os::windows::fs::symlink_dir(&req.target, &p)?;
        } else {
            std::os::windows::fs::symlink_file(&req.target, &p)?;
        }
        let m = fs::symlink_metadata(&p)?;
        Ok(stat_of(p, &m))
    }
}

/// Create a directory (with `parents`, `mkdir -p`). On Unix every directory is made with
/// `mkdirat` in a checked parent handle, one level at a time, so a directory swapped for a
/// symlink along the way is refused (#456).
#[cfg(unix)]
pub fn mkdir(policy: &PathPolicy, req: &MkdirRequest) -> Result<Stat, FsError> {
    let mode = req.mode.map(|m| m & 0o7777).unwrap_or(0o777) as libc::mode_t;
    if !req.parents {
        let p = policy.resolve_nofollow(&req.path)?;
        let par = open_parent(policy, p)?;
        at::mkdirat(&par.dir, &par.name, mode)?;
        let st = at::lstat(&par.dir, &par.name)?;
        return Ok(stat_raw(par.path, &st));
    }
    let p = policy.resolve_for_write(&req.path)?;
    if policy.is_root(&p) {
        let m = fs::metadata(&p)?;
        return Ok(stat_of(p, &m));
    }
    // The levels that do not exist yet, outermost first. Which ones they are is read by path;
    // each is then created through its checked parent, so a wrong guess only fails.
    let mut missing = Vec::new();
    let mut cur = p.as_path();
    while fs::symlink_metadata(cur).is_err() {
        missing.push(cur.to_path_buf());
        cur = match cur.parent() {
            Some(c) => c,
            None => break,
        };
    }
    missing.reverse();
    if missing.is_empty() {
        missing.push(p.clone());
    }
    let mut last = None;
    for level in missing {
        let par = open_parent(policy, level)?;
        match at::mkdirat(&par.dir, &par.name, mode) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        let st = at::lstat(&par.dir, &par.name)?;
        if st.st_mode as u32 & libc::S_IFMT as u32 != libc::S_IFDIR as u32 {
            return Err(io_err(libc::EEXIST, format!("{} exists and is not a directory", par.path.display())));
        }
        last = Some(stat_raw(par.path, &st));
    }
    last.ok_or_else(|| FsError::BadRequest("nothing to create".into()))
}

/// The path-based `mkdir` (Windows), with the parent resolved again before the call.
#[cfg(not(unix))]
pub fn mkdir(policy: &PathPolicy, req: &MkdirRequest) -> Result<Stat, FsError> {
    let p = if req.parents { policy.resolve_for_write(&req.path)? } else { policy.resolve_nofollow(&req.path)? };
    if !req.parents {
        recheck_parent(policy, &p)?;
    }
    let mut b = fs::DirBuilder::new();
    b.recursive(req.parents);
    b.create(&p)?;
    if req.parents {
        // Re-check after creating: a symlink planted along the way must not take us outside.
        policy.resolve_existing(&p)?;
    }
    let m = fs::symlink_metadata(&p)?;
    Ok(stat_of(p, &m))
}

/// Remove a file, a symbolic link (never its target) or a directory. On Unix the entry is
/// removed with `unlinkat` in its checked parent, and a recursive removal walks the tree through
/// directory handles opened with `O_NOFOLLOW` (#456).
pub fn remove(policy: &PathPolicy, req: &RemoveRequest) -> Result<(), FsError> {
    let p = policy.resolve_nofollow(&req.path)?;
    if policy.is_root(&p) {
        return Err(io_err(libc::EBUSY, format!("{} is an allowed root", p.display())));
    }
    #[cfg(unix)]
    {
        let par = open_parent(policy, p)?;
        let st = at::lstat(&par.dir, &par.name)?;
        if st.st_mode as u32 & libc::S_IFMT as u32 == libc::S_IFDIR as u32 {
            if req.recursive {
                at::remove_tree(&par.dir, &par.name)?;
            } else {
                at::unlinkat(&par.dir, &par.name, libc::AT_REMOVEDIR)?;
            }
        } else {
            at::unlinkat(&par.dir, &par.name, 0)?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        recheck_parent(policy, &p)?;
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
}

/// Rename within the roots. On Unix both parents are opened and checked and the move is one
/// `renameat` between the two handles (#456). Without `overwrite`, the existence check is not
/// atomic with the rename (as documented on [`RenameRequest`]).
pub fn rename(policy: &PathPolicy, req: &RenameRequest) -> Result<(), FsError> {
    let from = policy.resolve_nofollow(&req.from)?;
    let to = policy.resolve_nofollow(&req.to)?;
    if policy.is_root(&from) || policy.is_root(&to) {
        return Err(io_err(libc::EBUSY, "an allowed root cannot be renamed or replaced".into()));
    }
    #[cfg(unix)]
    {
        let src = open_parent(policy, from)?;
        let dst = open_parent(policy, to)?;
        at::lstat(&src.dir, &src.name)?;
        if !req.overwrite && at::lstat(&dst.dir, &dst.name).is_ok() {
            return Err(io_err(libc::EEXIST, format!("{} exists", dst.path.display())));
        }
        at::renameat(&src.dir, &src.name, &dst.dir, &dst.name)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        recheck_parent(policy, &from)?;
        recheck_parent(policy, &to)?;
        fs::symlink_metadata(&from)?;
        if !req.overwrite && fs::symlink_metadata(&to).is_ok() {
            return Err(io_err(libc::EEXIST, format!("{} exists", to.display())));
        }
        fs::rename(&from, &to)?;
        Ok(())
    }
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

    let existing_mode = match at::lstat(&d, &name).map(|st| st.st_mode as u32) {
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
        at::renameat(&d, &tmp, &d, &name)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = at::unlinkat(&d, &tmp, 0);
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

/// The entries of a directory. On Unix the directory is opened (`O_DIRECTORY | O_NOFOLLOW`) and
/// checked, and its names and their attributes are read from that handle (`fdopendir`,
/// `fstatat`), so a directory swapped for a symlink cannot list outside the roots (#456).
#[cfg(unix)]
pub fn list(policy: &PathPolicy, path: &Path) -> Result<ListResponse, FsError> {
    let p = policy.resolve_existing(path)?;
    let d = at::open_dir(&p)?;
    let p = policy.check_opened(&d, &p)?;
    let mut entries = Vec::new();
    for name in at::read_names(&d)? {
        // A denied entry is not listed, so the listing does not reveal the exclusion (#534).
        if policy.is_denied(&p.join(&name)) {
            continue;
        }
        let Ok(st) = at::lstat(&d, &name) else { continue };
        let s = stat_raw(PathBuf::new(), &st);
        entries.push(DirEntry {
            name: name.to_string_lossy().into_owned(),
            kind: s.kind,
            size: s.size,
            mtime_ms: s.mtime_ms,
            mode: Some(s.mode),
            ino: s.ino,
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(ListResponse { path: p, entries })
}

/// The path-based listing (Windows).
#[cfg(not(unix))]
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
            mode: None,
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
        // The walk reads directories by path, so a directory swapped for a symlink mid-walk
        // could list outside the root. Every match is read again with the checked [`lstat`]
        // (its parent opened and checked), and its mtime comes from that answer (#456). A name
        // that exists both outside and at the same place inside is not told apart.
        if m.is_match(rel) {
            let Ok(s) = lstat(policy, e.path()) else { continue };
            let mt = s.mtime_ms;
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

