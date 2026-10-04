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

use std::fs::{self, File, Metadata};
use std::io::{self, Read, Seek, SeekFrom, Write};
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
    let mut f = File::open(path)?;
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

fn ms_of(secs: i64, nsecs: i64) -> Option<u64> {
    (secs >= 0).then(|| secs as u64 * 1000 + (nsecs.max(0) as u64) / 1_000_000)
}

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
        libc::ETXTBSY => "ETXTBSY",
        libc::EFBIG => "EFBIG",
        libc::ENOSPC => "ENOSPC",
        libc::EROFS => "EROFS",
        libc::EMLINK => "EMLINK",
        libc::ENAMETOOLONG => "ENAMETOOLONG",
        libc::ENOTEMPTY => "ENOTEMPTY",
        libc::ELOOP => "ELOOP",
        libc::EDQUOT => "EDQUOT",
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
    std::os::unix::fs::symlink(&req.target, &p)?;
    let m = fs::symlink_metadata(&p)?;
    Ok(stat_of(p, &m))
}

pub fn mkdir(policy: &PathPolicy, req: &MkdirRequest) -> Result<Stat, FsError> {
    use std::os::unix::fs::DirBuilderExt;
    let p = if req.parents { policy.resolve_for_write(&req.path)? } else { policy.resolve_nofollow(&req.path)? };
    let mut b = fs::DirBuilder::new();
    b.recursive(req.parents);
    if let Some(mode) = req.mode {
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
pub fn setattr(policy: &PathPolicy, req: &SetAttrRequest) -> Result<Stat, FsError> {
    let p = policy.resolve_existing(&req.path)?;
    if let Some(size) = req.size {
        let m = fs::metadata(&p)?;
        if m.is_dir() {
            return Err(io_err(libc::EISDIR, format!("{} is a directory", p.display())));
        }
        fs::OpenOptions::new().write(true).open(&p)?.set_len(size)?;
    }
    if let Some(mode) = req.mode {
        fs::set_permissions(&p, fs::Permissions::from_mode(mode & 0o7777))?;
    }
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
    let m = fs::metadata(&p)?;
    Ok(stat_of(p, &m))
}

/// Positional in-place write into an existing regular file (see [`PwriteRequest`]).
pub fn pwrite(policy: &PathPolicy, req: &PwriteRequest) -> Result<Stat, FsError> {
    use std::os::unix::fs::FileExt;
    let p = policy.resolve_existing(&req.path)?;
    let f = fs::OpenOptions::new().write(true).open(&p)?;
    let m = f.metadata()?;
    if !m.is_file() {
        return Err(io_err(libc::EINVAL, format!("{} is not a regular file", p.display())));
    }
    f.write_all_at(&req.data, req.offset)?;
    let m = f.metadata()?;
    Ok(stat_of(p, &m))
}

pub fn read(policy: &PathPolicy, req: &ReadRequest) -> Result<ReadResponse, FsError> {
    let p = policy.resolve_existing(&req.path)?;
    let mut f = File::open(&p)?;
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
    let sha256 = if req.hash { Some(sha256_file(&p)?) } else { None };
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
            f.set_permissions(fs::Permissions::from_mode(m.permissions().mode()))?;
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
        let Ok(m) = e.metadata() else { continue };
        entries.push(DirEntry {
            name: e.file_name().to_string_lossy().into_owned(),
            kind: kind_of(m.file_type()),
            size: m.len(),
            mtime_ms: mtime_ms(&m),
            mode: Some(m.permissions().mode()),
            ino: Some(m.ino()),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(ListResponse { path: p, entries })
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
fn walk(root: &Path, respect_ignore: bool) -> impl Iterator<Item = ignore::DirEntry> {
    ignore::WalkBuilder::new(root)
        .standard_filters(respect_ignore)
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
    for e in walk(&root, req.respect_ignore) {
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
        Box::new(walk(&root, req.respect_ignore).map(|e| e.into_path()))
    };
    'files: for path in files {
        if let Some(f) = &filter {
            let rel = path.strip_prefix(&root).unwrap_or(&path);
            // Match the relative path or the bare file name, as `rg --glob '*.rs'` does.
            if !f.is_match(rel) && !path.file_name().is_some_and(|n| f.is_match(n)) {
                continue;
            }
        }
        let mut hit_limit = false;
        let res = searcher.search_path(
            &re,
            &path,
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

