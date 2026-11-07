//! What a node keeps after it enrolled with an ember server (SPEC `FR-C2h`): `enrol.json` in the
//! state dir (the server-minted node token and where it came from), and the server's peer id
//! appended to `allowed-peers`.
//!
//! This module is storage only (N1 of the node enrol design): reading and validating the file,
//! and writing it together with `allowed-peers` so that both change or neither does, under a lock
//! on the state dir ([`lock_state_dir`]). The `ember-node enrol` command that fetches the
//! credential is [`crate::enrol_cli`] (N2). Unix only: the credential file is protected by mode 0600 and
//! the owner check, with no Windows ACL handling.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// `<state dir>/enrol.json`.
pub const ENROL_FILE: &str = "enrol.json";

/// The file is a few hundred bytes; anything larger is not ours.
const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_TOKEN_CHARS: usize = 512;

/// `<state dir>/.enrol.lock`, held (flock) for the whole of one enrol.
pub const LOCK_FILE: &str = ".enrol.lock";

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Warns (path only) when an existing state dir is not ours or is writable by group or others.
/// It never refuses and never changes the mode: an existing setup must keep working.
fn warn_state_dir(dir: &Path) {
    if let Ok(m) = fs::metadata(dir) {
        if m.uid() != euid() || m.mode() & 0o022 != 0 {
            tracing::warn!(path = %dir.display(), "state directory is not owned by this user or is writable by group or others");
        }
    }
}

/// Opens an existing file for reading without following a symlink and without blocking on a FIFO,
/// and requires a regular file owned by this user. Errors name the path and the reason, never the
/// contents. A missing file is a plain `io::Error` of kind `NotFound` (see [`is_not_found`]).
fn open_regular_owned(path: &Path) -> anyhow::Result<fs::File> {
    let file = match OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(anyhow::Error::new(e)),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => anyhow::bail!("{} is a symbolic link", path.display()),
        Err(e) => return Err(anyhow::Error::new(e).context(format!("cannot open {}", path.display()))),
    };
    let meta = file.metadata().with_context(|| format!("cannot stat {}", path.display()))?;
    anyhow::ensure!(meta.file_type().is_file(), "{} is not a regular file", path.display());
    anyhow::ensure!(meta.uid() == euid(), "{} is not owned by this user", path.display());
    Ok(file)
}

fn is_not_found(err: &anyhow::Error) -> bool {
    err.downcast_ref::<io::Error>().is_some_and(|e| e.kind() == io::ErrorKind::NotFound)
}

/// Reads at most [`MAX_FILE_BYTES`]; more is an error.
fn read_capped(file: fs::File, path: &Path) -> anyhow::Result<Vec<u8>> {
    let mut data = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut data).with_context(|| format!("cannot read {}", path.display()))?;
    anyhow::ensure!(data.len() as u64 <= MAX_FILE_BYTES, "{} is larger than {} bytes", path.display(), MAX_FILE_BYTES);
    Ok(data)
}

/// The lock file must be a regular file owned by this user: a file another user planted there
/// (in a group or other writable dir) could otherwise hold the lock and block every enrol. Only a
/// denial of service, but cheap to refuse.
fn check_lock_file(is_file: bool, uid: u32, path: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(is_file, "{} is not a regular file", path.display());
    anyhow::ensure!(uid == euid(), "{} is not owned by this user", path.display());
    Ok(())
}

/// The exclusive lock on a state dir, held while one enrol runs. Released when dropped (the file
/// descriptor closes).
#[derive(Debug)]
pub struct EnrolLock {
    _file: fs::File,
    dir: PathBuf,
}

impl EnrolLock {
    /// The locked state dir.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Locks `<state dir>/.enrol.lock` (`flock`, exclusive, non-blocking), creating the state dir with
/// mode 0700 when it is missing (an existing dir is never chmodded). Another enrol holding the
/// lock is an error. The lock file is opened with `O_NOFOLLOW`, mode 0600, and must be a regular
/// file owned by this user.
///
/// The lock is per host: `flock` on a state dir on NFS may be advisory-only or unsupported, and
/// then two enrols on different machines are not kept apart. Readers ([`read_enrolment`], and so
/// `config::from_env`) do not take the lock on purpose: replacing `enrol.json` is one `rename`, so
/// a reader sees either the old or the new complete file.
pub fn lock_state_dir(state_dir: &Path) -> anyhow::Result<EnrolLock> {
    if state_dir.exists() {
        warn_state_dir(state_dir);
    } else {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(state_dir)
            .with_context(|| format!("cannot create {}", state_dir.display()))?;
    }
    let path = state_dir.join(LOCK_FILE);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let meta = file.metadata().with_context(|| format!("cannot stat {}", path.display()))?;
    check_lock_file(meta.file_type().is_file(), meta.uid(), &path)?;
    // Clear FD_CLOEXEC protection is already default in Rust std, but we also explicitly unlock
    // on drop so even if another thread forked while this lock was open, the flock is released
    // immediately when `EnrolLock` is dropped.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::WouldBlock {
            anyhow::bail!("another ember-node enrol is running for this state directory");
        }
        return Err(anyhow::Error::new(err).context(format!("cannot lock {}", path.display())));
    }
    Ok(EnrolLock { _file: file, dir: state_dir.to_path_buf() })
}

impl Drop for EnrolLock {
    fn drop(&mut self) {
        // Explicit LOCK_UN releases the kernel flock entry immediately even if the file descriptor
        // was duplicated across a concurrent fork in another test thread.
        unsafe {
            let _ = libc::flock(self._file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

/// The contents of `enrol.json`. `node_token` is a secret: `Debug` redacts it.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct Enrolment {
    /// The server's base URL as the node used it.
    pub server: String,
    /// The bearer token the server uses against this node.
    pub node_token: String,
    /// The server's transport peer id (64 lowercase hex), when the server delivered one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_peer_id: Option<String>,
    /// The computer name the owner chose.
    pub name: String,
    pub computer_id: String,
    pub enrolled_ms: i64,
}

impl fmt::Debug for Enrolment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Enrolment")
            .field("server", &self.server)
            .field("node_token", &"[redacted]")
            .field("server_peer_id", &self.server_peer_id)
            .field("name", &self.name)
            .field("computer_id", &self.computer_id)
            .field("enrolled_ms", &self.enrolled_ms)
            .finish()
    }
}

pub(crate) fn is_peer_hex(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Shape checks that never echo the value (the token is a secret).
pub(crate) fn validate(e: &Enrolment) -> anyhow::Result<()> {
    anyhow::ensure!(!e.node_token.is_empty(), "the node token is empty");
    anyhow::ensure!(e.node_token.chars().count() <= MAX_TOKEN_CHARS, "the node token is longer than {MAX_TOKEN_CHARS} characters");
    anyhow::ensure!(
        e.node_token.chars().all(|c| c.is_ascii_graphic()),
        "the node token must be printable ASCII without spaces"
    );
    if let Some(id) = &e.server_peer_id {
        anyhow::ensure!(is_peer_hex(id), "server_peer_id must be 64 lowercase hex characters");
    }
    Ok(())
}

/// Reads `<state dir>/enrol.json`. A missing file is `Ok(None)`. A symbolic link, a file that is
/// not regular or not owned by this user, an oversized, malformed or invalid file is an error that
/// names the file and never quotes its contents. A file readable by group or others, or a state
/// dir that is not ours or is group/other writable, only logs a warning (existing shared setups
/// must keep working).
pub fn read_enrolment(state_dir: &Path) -> anyhow::Result<Option<Enrolment>> {
    warn_state_dir(state_dir);
    let path = state_dir.join(ENROL_FILE);
    let file = match open_regular_owned(&path) {
        Ok(f) => f,
        Err(e) if is_not_found(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    let meta = file.metadata().with_context(|| format!("cannot stat {}", path.display()))?;
    anyhow::ensure!(meta.len() <= MAX_FILE_BYTES, "{} is larger than {} bytes", path.display(), MAX_FILE_BYTES);
    if meta.permissions().mode() & 0o077 != 0 {
        tracing::warn!(path = %path.display(), "enrol file is accessible to group or others; run chmod 600 on it");
    }
    let data = read_capped(file, &path)?;
    // The serde error is dropped on purpose: for a wrong type it can quote the value.
    let e: Enrolment = serde_json::from_slice(&data).map_err(|err| {
        anyhow::anyhow!("{} is malformed (not a valid enrolment file; line {}, column {})", path.display(), err.line(), err.column())
    })?;
    validate(&e).map_err(|err| anyhow::anyhow!("{} is invalid: {err}", path.display()))?;
    Ok(Some(e))
}

/// The new text of an `allowed-peers` file after adding `id`: `existing` byte for byte, then the
/// id on its own line. `Ok(None)` when `id` is already listed. An `existing` text that does not
/// parse is an error: what cannot be read is never overwritten.
pub fn append_allowed_peer(existing: &str, id: &str) -> anyhow::Result<Option<String>> {
    anyhow::ensure!(is_peer_hex(id), "a peer id is 64 lowercase hex characters");
    let new: ember_transport::PeerId = id.parse().map_err(|e| anyhow::anyhow!("peer id: {e}"))?;
    let listed = crate::transport::parse_peer_list(existing)?;
    if listed.contains(&new) {
        return Ok(None);
    }
    let mut text = String::with_capacity(existing.len() + 66);
    text.push_str(existing);
    if !existing.is_empty() && !existing.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(id);
    text.push('\n');
    Ok(Some(text))
}

/// Why [`write_enrolment`] failed, and what is on disk afterwards. The caller needs the
/// difference: the server hands a credential over once, so a failed write may have lost it.
/// `Display` is the inner error's full text (`{:#}`); there is no `source`, so a chain printed by
/// anyhow does not repeat it.
#[derive(Debug)]
pub enum WriteError {
    /// Nothing changed: validation, an existing file without `replace`, a symlinked or foreign
    /// `allowed-peers`, the lock, staging, or the first rename failing with nothing moved.
    Refused(anyhow::Error),
    /// A rename failed after the first one and the previous state was restored.
    RolledBack(anyhow::Error),
    /// The undo failed too: `enrol.json` now holds the NEW enrolment.
    NewKept(anyhow::Error),
    /// Both files are in place; only the final directory fsync failed. Nothing is undone. The
    /// report says what the write did, so the caller can still tell the user about a new peer.
    NotSynced(anyhow::Error, WriteReport),
}

impl WriteError {
    fn inner(&self) -> &anyhow::Error {
        match self {
            WriteError::Refused(e) | WriteError::RolledBack(e) | WriteError::NewKept(e) | WriteError::NotSynced(e, _) => e,
        }
    }
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#}", self.inner())
    }
}

impl std::error::Error for WriteError {}

/// What a successful [`write_enrolment`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteReport {
    /// A NEW line was appended to `allowed-peers` (false when no peer was asked for, or the id was
    /// already listed).
    pub peer_added: bool,
}

type SyncDir<'a> = &'a dyn Fn(&Path) -> io::Result<()>;
type Rename<'a> = &'a dyn Fn(&Path, &Path) -> io::Result<()>;

/// Files made for one write, before any of them is renamed into place.
struct Staged {
    enrol_tmp: PathBuf,
    enrol_dst: PathBuf,
    /// The `allowed-peers` temp and its destination, when that file changes.
    peers: Option<(PathBuf, PathBuf)>,
}

impl Staged {
    fn remove_temps(&self) {
        let _ = fs::remove_file(&self.enrol_tmp);
        if let Some((tmp, _)) = &self.peers {
            let _ = fs::remove_file(tmp);
        }
    }
}

/// 16 hex characters, unique per call: a v4 UUID (uuid is already a dependency).
fn suffix() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_string()
}

fn temp_name(dir: &Path, name: &str, suffix: &str) -> PathBuf {
    dir.join(format!("{name}.{suffix}.tmp"))
}

/// Creates `<dir>/<name>.<suffix>.tmp` with `create_new` (O_EXCL: an existing file or a planted
/// symlink at that name is an error, never followed), writes `bytes`, sets `mode`, syncs.
fn stage_file(dir: &Path, name: &str, suffix: &str, bytes: &[u8], mode: u32) -> io::Result<PathBuf> {
    let path = temp_name(dir, name, suffix);
    let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
    let result = (|| {
        f.write_all(bytes)?;
        f.set_permissions(fs::Permissions::from_mode(mode))?;
        f.sync_all()
    })();
    match result {
        Ok(()) => Ok(path),
        Err(e) => {
            drop(f);
            let _ = fs::remove_file(&path);
            Err(e)
        }
    }
}

/// [`stage_file`] with a fresh suffix, tried up to 5 times when the name is taken.
fn stage_retry(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> anyhow::Result<PathBuf> {
    let mut last = None;
    for _ in 0..5 {
        match stage_file(dir, name, &suffix(), bytes, mode) {
            Ok(p) => return Ok(p),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(anyhow::Error::new(e).context(format!("cannot stage {name} in {}", dir.display()))),
        }
    }
    let e = last.unwrap_or_else(|| io::Error::from(io::ErrorKind::AlreadyExists));
    Err(anyhow::Error::new(e).context(format!("cannot find a free temporary name for {name} in {}", dir.display())))
}

/// Flushes the directory entries (the renames and removals) to disk.
fn sync_dir(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

/// The previous `enrol.json` could not be put back: say plainly what is on disk now.
fn put_back(
    stage: &Staged,
    previous: Option<&(Vec<u8>, u32)>,
    dir: &Path,
    rename: Rename,
    sync: SyncDir,
    cause: anyhow::Error,
) -> WriteError {
    let restored: anyhow::Result<()> = match previous {
        Some((bytes, mode)) => stage_retry(dir, ENROL_FILE, bytes, *mode).and_then(|tmp| {
            rename(&tmp, &stage.enrol_dst).map_err(|e| {
                let _ = fs::remove_file(&tmp);
                anyhow::Error::new(e)
            })
        }),
        None => match fs::remove_file(&stage.enrol_dst) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other.map_err(anyhow::Error::new),
        },
    };
    match restored {
        Ok(()) => match sync(dir) {
            Ok(()) => WriteError::RolledBack(cause),
            Err(e) => WriteError::RolledBack(
                cause.context(format!("the previous state was restored, but syncing {} failed ({e})", dir.display())),
            ),
        },
        Err(e) => WriteError::NewKept(cause.context(format!(
            "enrol.json now holds the NEW enrolment: the previous state could not be restored ({e:#})"
        ))),
    }
}

/// Renames the staged files into place, `enrol.json` first. Replacing `enrol.json` is one atomic
/// `rename`, so the file never goes missing. If the `allowed-peers` rename fails, the previous
/// bytes and mode are staged again and renamed over `enrol.json` (or the new file is removed when
/// there was none); no backup file is ever made. The directory is synced after the renames; a
/// failed sync is [`WriteError::NotSynced`] and nothing is undone. `sync_all` on macOS is fsync,
/// it does not issue `F_FULLFSYNC` (a known limit, the call is not added).
fn commit(
    stage: &Staged,
    previous: Option<&(Vec<u8>, u32)>,
    dir: &Path,
    rename: Rename,
    sync: SyncDir,
) -> Result<(), WriteError> {
    if let Err(e) = rename(&stage.enrol_tmp, &stage.enrol_dst) {
        // Nothing changed: the previous enrol.json, if any, is still in place.
        return Err(WriteError::Refused(anyhow::Error::new(e).context("cannot move enrol.json into place")));
    }
    if let Some((tmp, dst)) = &stage.peers {
        if let Err(e) = rename(tmp, dst) {
            let cause = anyhow::Error::new(e).context("cannot update allowed-peers");
            return Err(put_back(stage, previous, dir, rename, sync, cause));
        }
    }
    // The data renames succeeded; a failed sync is reported but the files are kept.
    // On macOS `sync_all` is fsync, not F_FULLFSYNC: the drive may still hold the data in its cache.
    sync(dir).map_err(|e| {
        WriteError::NotSynced(
            anyhow::anyhow!("enrol.json was written, but syncing {} failed ({e}); the files are kept", dir.display()),
            WriteReport { peer_added: stage.peers.is_some() },
        )
    })
}

/// Writes `enrol.json` and, with `allow_peer`, adds that peer id to `allowed-peers`, so that both
/// change or neither does (SPEC `FR-C2h`). The caller holds the [`EnrolLock`] for the whole run;
/// every check below happens under it.
///
/// An existing `enrol.json` is an error unless `replace`. Both files are staged first (mode 0600
/// for `enrol.json`, `create_new`, synced), then `enrol.json` is renamed into place (atomically
/// replacing any previous one), then `allowed-peers`. If the second rename fails, the previous
/// `enrol.json` (read into memory beforehand) is staged again and renamed back, or the new one is
/// removed. An `enrol.json` without the peer is harmless (the HTTP path works); a peer without a
/// token is not, hence the order. `allowed-peers` keeps its text byte for byte and its mode; a
/// symlink, a non-regular file or a file of another user there is refused.
pub fn write_enrolment(
    lock: &EnrolLock,
    e: &Enrolment,
    allow_peer: Option<&str>,
    replace: bool,
) -> Result<WriteReport, WriteError> {
    write_with(lock, e, allow_peer, replace, &|from, to| fs::rename(from, to), &sync_dir)
}

fn write_with(
    lock: &EnrolLock,
    e: &Enrolment,
    allow_peer: Option<&str>,
    replace: bool,
    rename: Rename,
    sync: SyncDir,
) -> Result<WriteReport, WriteError> {
    let (stage, previous) = prepare(lock, e, allow_peer, replace).map_err(WriteError::Refused)?;
    let result = commit(&stage, previous.as_ref(), lock.dir(), rename, sync);
    stage.remove_temps();
    result.map(|()| WriteReport { peer_added: stage.peers.is_some() })
}

/// The existing `enrol.json` as `write_with` keeps it for a roll back: its bytes and mode. The
/// same checks run in [`preflight`] before the redeem, so the two cannot drift. `open_regular_owned`
/// already refuses a link, a non-regular file and a foreign owner; the caller's own `lstat` checks
/// elsewhere are kept as a second layer on purpose (defence in depth), not because either alone is
/// insufficient.
fn read_previous_enrol(path: &Path) -> anyhow::Result<(Vec<u8>, u32)> {
    let file = open_regular_owned(path)?;
    let mode = file.metadata().with_context(|| format!("cannot stat {}", path.display()))?.permissions().mode() & 0o777;
    Ok((read_capped(file, path)?, mode))
}

/// The existing `allowed-peers` as (text, mode), `Ok(None)` when it is missing. A symlink, a
/// non-regular file, a file of another user, an oversized or non-UTF-8 file is an error. Shared by
/// [`prepare`] and [`preflight`] so the two cannot drift. The `lstat` checks here and the
/// `O_NOFOLLOW` open in `open_regular_owned` overlap on purpose: two layers (defence in depth),
/// the first gives the clearer message, the second closes the window between stat and open.
fn read_peers_file(peers_dst: &Path) -> anyhow::Result<Option<(String, u32)>> {
    match fs::symlink_metadata(peers_dst) {
        Ok(m) => {
            anyhow::ensure!(
                !m.file_type().is_symlink(),
                "{} is a symbolic link; it is not replaced (point the state directory at the real file)",
                peers_dst.display()
            );
            anyhow::ensure!(m.file_type().is_file(), "{} is not a regular file; it is not replaced", peers_dst.display());
            anyhow::ensure!(m.uid() == euid(), "{} is not owned by this user; it is not replaced", peers_dst.display());
            let file = open_regular_owned(peers_dst)?;
            let mode = file.metadata().map(|m| m.permissions().mode() & 0o777).unwrap_or(0o600);
            // Capped like enrol.json: a bigger file is refused, never read whole.
            let bytes = read_capped(file, peers_dst)?;
            let text = String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("{} is not valid UTF-8", peers_dst.display()))?;
            Ok(Some((text, mode)))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(anyhow::Error::new(err).context(format!("cannot stat {}", peers_dst.display()))),
    }
}

/// Refuses, before the server hands the credential over, what [`write_enrolment`] would refuse
/// afterwards (the server gives the credential out once, so a refusal after delivery loses it).
/// Under the same lock; changes nothing and leaves no file behind. Checks:
/// - with `replace`, an existing `enrol.json` that the hardened reader cannot read (link, not
///   regular, foreign owner, over 64 KiB). Without `replace` the plain "already exists" refusal
///   stays in the caller.
/// - an existing `allowed-peers` that is a link, not regular, foreign, over 64 KiB, not UTF-8, or
///   does not parse. It is checked whether or not the server will turn out to be trusted: whether
///   the id is added is only known after the delivery.
/// - the state dir is writable (a `.enrol-preflight.<16 hex>.tmp` is created with mode 0600 and
///   removed again).
pub fn preflight(lock: &EnrolLock, replace: bool) -> anyhow::Result<()> {
    let dir = lock.dir();
    let enrol_dst = dir.join(ENROL_FILE);
    if replace {
        match fs::symlink_metadata(&enrol_dst) {
            Ok(_) => {
                read_previous_enrol(&enrol_dst).map_err(|e| {
                    anyhow::anyhow!("cannot replace the existing enrolment: {e:#}; move or remove that file, then run enrol again")
                })?;
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(anyhow::Error::new(err).context(format!("cannot stat {}", enrol_dst.display())));
            }
        }
    }
    let peers_dst = crate::transport::allowed_peers_file(dir);
    let fix = |e: anyhow::Error| {
        anyhow::anyhow!("{e:#}; fix or move that file, then run enrol again (nothing was redeemed)")
    };
    if let Some((text, _)) = read_peers_file(&peers_dst).map_err(fix)? {
        crate::transport::parse_peer_list(&text).map_err(|e| fix(e.context(format!("{} does not parse", peers_dst.display()))))?;
    }
    let probe = temp_name(dir, ".enrol-preflight", &suffix());
    match OpenOptions::new().write(true).create_new(true).mode(0o600).open(&probe) {
        Ok(f) => {
            drop(f);
            let _ = fs::remove_file(&probe);
        }
        Err(e) => {
            return Err(anyhow::Error::new(e).context(format!(
                "the state directory {} is not writable; fix its permissions, then run enrol again (nothing was redeemed)",
                dir.display()
            )));
        }
    }
    Ok(())
}

/// Every check and the staging of both files, under the lock. Anything that fails here changed
/// nothing (staged temps are removed).
fn prepare(
    lock: &EnrolLock,
    e: &Enrolment,
    allow_peer: Option<&str>,
    replace: bool,
) -> anyhow::Result<(Staged, Option<(Vec<u8>, u32)>)> {
    validate(e)?;
    let dir = lock.dir();
    let enrol_dst = dir.join(ENROL_FILE);
    let exists = match fs::symlink_metadata(&enrol_dst) {
        Ok(_) => true,
        Err(err) if err.kind() == io::ErrorKind::NotFound => false,
        Err(err) => return Err(anyhow::Error::new(err).context(format!("cannot stat {}", enrol_dst.display()))),
    };
    if exists && !replace {
        anyhow::bail!("{} already exists; pass --replace to replace it", enrol_dst.display());
    }
    // What to put back if the second rename fails.
    let previous: Option<(Vec<u8>, u32)> = if exists { Some(read_previous_enrol(&enrol_dst)?) } else { None };

    // The new allowed-peers text, decided before anything is created.
    let peers_dst = crate::transport::allowed_peers_file(dir);
    let mut peers_new: Option<(String, u32)> = None;
    if let Some(id) = allow_peer {
        let (existing, mode) = read_peers_file(&peers_dst)?.unwrap_or((String::new(), 0o600));
        if let Some(text) = append_allowed_peer(&existing, id)? {
            peers_new = Some((text, mode));
        }
    }

    let json = serde_json::to_vec_pretty(e).context("cannot encode enrol.json")?;
    let enrol_tmp = stage_retry(dir, ENROL_FILE, &json, 0o600)?;
    let peers = match peers_new {
        None => None,
        Some((text, mode)) => match stage_retry(dir, crate::transport::ALLOWED_PEERS_FILE, text.as_bytes(), mode) {
            Ok(tmp) => Some((tmp, peers_dst)),
            Err(err) => {
                let _ = fs::remove_file(&enrol_tmp);
                return Err(err);
            }
        },
    };
    Ok((Staged { enrol_tmp, enrol_dst, peers }, previous))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn peer() -> String {
        ember_transport::SecretKey::generate().peer_id().to_string()
    }

    fn sample() -> Enrolment {
        Enrolment {
            server: "https://ember.example".into(),
            node_token: "tok_SECRET-value.123".into(),
            server_peer_id: None,
            name: "build box".into(),
            computer_id: "c1".into(),
            enrolled_ms: 1_700_000_000_000,
        }
    }

    /// Directory entries other than the lock file, sorted.
    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != LOCK_FILE)
            .collect();
        v.sort();
        v
    }

    /// Locks the dir, writes, unlocks.
    fn put(dir: &Path, e: &Enrolment, peer: Option<&str>, replace: bool) -> anyhow::Result<WriteReport> {
        let lock = lock_state_dir(dir)?;
        Ok(write_enrolment(&lock, e, peer, replace)?)
    }

    fn put_with(
        dir: &Path,
        e: &Enrolment,
        peer: Option<&str>,
        replace: bool,
        rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
    ) -> Result<WriteReport, WriteError> {
        let lock = lock_state_dir(dir).unwrap();
        write_with(&lock, e, peer, replace, rename, &sync_dir)
    }

    fn put_sync(
        dir: &Path,
        e: &Enrolment,
        peer: Option<&str>,
        sync: &dyn Fn(&Path) -> io::Result<()>,
    ) -> Result<WriteReport, WriteError> {
        let lock = lock_state_dir(dir).unwrap();
        write_with(&lock, e, peer, false, &|from, to| fs::rename(from, to), sync)
    }

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn fr_c2_node_read_missing_file_is_none() {
        let d = tempfile::tempdir().unwrap();
        assert!(read_enrolment(d.path()).unwrap().is_none());
    }

    #[test]
    fn fr_c2_node_read_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let mut e = sample();
        e.server_peer_id = Some(peer());
        put(d.path(), &e, None, false).unwrap();
        assert_eq!(read_enrolment(d.path()).unwrap(), Some(e));
        let e2 = sample();
        put(d.path(), &e2, None, true).unwrap();
        let text = fs::read_to_string(d.path().join(ENROL_FILE)).unwrap();
        assert!(!text.contains("server_peer_id"), "an absent peer id is not serialized");
    }

    #[test]
    fn fr_c2_node_read_malformed_names_the_file_and_never_quotes_it() {
        let d = tempfile::tempdir().unwrap();
        let body = r#"{"server": 12, "node_token": "tok_SECRET-value.123", "name": "x" "#;
        fs::write(d.path().join(ENROL_FILE), body).unwrap();
        let err = format!("{:#}", read_enrolment(d.path()).unwrap_err());
        assert!(err.contains("enrol.json") && err.contains("malformed"), "{err}");
        assert!(!err.contains("tok_SECRET"), "{err}");
        // A well-formed document of the wrong type must not quote the value either.
        let body = r#"{"server":"s","node_token":"tok_SECRET-value.123","name":"n","computer_id":"c","enrolled_ms":"tok_SECRET-value.123"}"#;
        fs::write(d.path().join(ENROL_FILE), body).unwrap();
        let err = format!("{:#}", read_enrolment(d.path()).unwrap_err());
        assert!(err.contains("malformed") && !err.contains("tok_SECRET"), "{err}");
    }

    #[test]
    fn fr_c2_node_read_oversized_file_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join(ENROL_FILE), vec![b' '; (MAX_FILE_BYTES + 1) as usize]).unwrap();
        let err = format!("{:#}", read_enrolment(d.path()).unwrap_err());
        assert!(err.contains("larger than"), "{err}");
    }

    #[test]
    fn fr_c2_node_read_rejects_bad_tokens() {
        let d = tempfile::tempdir().unwrap();
        for bad in ["", "has space", "tab\there", "caf\u{e9}", "a".repeat(MAX_TOKEN_CHARS + 1).as_str()] {
            let mut e = sample();
            e.node_token = bad.to_string();
            fs::write(d.path().join(ENROL_FILE), serde_json::to_vec(&e).unwrap()).unwrap();
            let err = format!("{:#}", read_enrolment(d.path()).unwrap_err());
            assert!(err.contains("token") && err.contains("enrol.json"), "{err}");
            if !bad.is_empty() {
                assert!(!err.contains(bad), "the token is never echoed");
            }
        }
        let mut ok = sample();
        ok.node_token = "a".repeat(MAX_TOKEN_CHARS);
        fs::write(d.path().join(ENROL_FILE), serde_json::to_vec(&ok).unwrap()).unwrap();
        assert!(read_enrolment(d.path()).unwrap().is_some());
    }

    #[test]
    fn fr_c2_node_read_rejects_bad_server_peer_id() {
        let d = tempfile::tempdir().unwrap();
        for bad in ["abc", "A".repeat(64).as_str(), "g".repeat(64).as_str(), "a".repeat(63).as_str()] {
            let mut e = sample();
            e.server_peer_id = Some(bad.to_string());
            fs::write(d.path().join(ENROL_FILE), serde_json::to_vec(&e).unwrap()).unwrap();
            let err = format!("{:#}", read_enrolment(d.path()).unwrap_err());
            assert!(err.contains("server_peer_id"), "{err}");
        }
    }

    #[test]
    fn fr_c2_node_debug_redacts_the_token() {
        let shown = format!("{:?} {:#?}", sample(), sample());
        assert!(!shown.contains("tok_SECRET"), "{shown}");
        assert!(shown.contains("redacted") && shown.contains("build box"));
    }

    #[test]
    fn fr_c2_node_append_keeps_text_and_adds_its_own_line() {
        let a = peer();
        let b = peer();
        let existing = format!("# my servers\n{a}  # home\n\n");
        let out = append_allowed_peer(&existing, &b).unwrap().unwrap();
        assert_eq!(out, format!("{existing}{b}\n"));
        assert!(out.starts_with(&existing), "existing text is kept byte for byte");
    }

    #[test]
    fn fr_c2_node_append_adds_a_newline_when_the_text_has_none() {
        let a = peer();
        let b = peer();
        let out = append_allowed_peer(&a, &b).unwrap().unwrap();
        assert_eq!(out, format!("{a}\n{b}\n"));
        assert_eq!(append_allowed_peer("", &b).unwrap().unwrap(), format!("{b}\n"));
    }

    #[test]
    fn fr_c2_node_append_does_not_duplicate() {
        let a = peer();
        assert!(append_allowed_peer(&format!("{a}\n"), &a).unwrap().is_none());
        assert!(append_allowed_peer(&format!("x\n{a}"), &a).is_err(), "unparsable text is an error before anything else");
        let b = peer();
        assert!(append_allowed_peer(&format!("{a}, {b} # both\n"), &b).unwrap().is_none());
    }

    #[test]
    fn fr_c2_node_append_refuses_unparsable_existing_text_and_bad_ids() {
        let a = peer();
        assert!(append_allowed_peer("not-a-key\n", &a).is_err());
        for bad in ["", "abc", a.to_uppercase().as_str(), format!("{a}0").as_str()] {
            assert!(append_allowed_peer("", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn fr_c2_node_write_makes_enrol_json_0600_and_no_allowed_peers_without_a_peer() {
        let d = tempfile::tempdir().unwrap();
        put(d.path(), &sample(), None, false).unwrap();
        assert_eq!(mode(&d.path().join(ENROL_FILE)), 0o600);
        assert!(!crate::transport::allowed_peers_file(d.path()).exists());
        assert_eq!(names(d.path()), vec![ENROL_FILE.to_string()], "no temp files remain");
    }

    #[test]
    fn fr_c2_node_write_creates_a_missing_state_dir_0700() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("a").join("node");
        put(&dir, &sample(), None, false).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(LOCK_FILE)), 0o600);
        assert!(dir.join(ENROL_FILE).exists());
    }

    #[test]
    fn fr_c2_node_write_adds_exactly_the_one_peer_and_keeps_the_old_text_and_mode() {
        let d = tempfile::tempdir().unwrap();
        let old = peer();
        let new = peer();
        let path = crate::transport::allowed_peers_file(d.path());
        fs::write(&path, format!("# keep\n{old}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        put(d.path(), &sample(), Some(&new), false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), format!("# keep\n{old}\n{new}\n"));
        assert_eq!(mode(&path), 0o640, "the mode of an existing file is kept");
        assert_eq!(names(d.path()), vec!["allowed-peers".to_string(), ENROL_FILE.to_string()]);
    }

    #[test]
    fn fr_c2_node_write_creates_allowed_peers_0600_with_only_the_new_id() {
        let d = tempfile::tempdir().unwrap();
        let new = peer();
        put(d.path(), &sample(), Some(&new), false).unwrap();
        let path = crate::transport::allowed_peers_file(d.path());
        assert_eq!(fs::read_to_string(&path).unwrap(), format!("{new}\n"));
        assert_eq!(mode(&path), 0o600);
    }

    #[test]
    fn fr_c2_node_write_leaves_allowed_peers_alone_when_the_peer_is_listed() {
        let d = tempfile::tempdir().unwrap();
        let a = peer();
        let path = crate::transport::allowed_peers_file(d.path());
        fs::write(&path, format!("{a}\n")).unwrap();
        put(d.path(), &sample(), Some(&a), false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), format!("{a}\n"));
        assert_eq!(names(d.path()).len(), 2);
    }

    #[test]
    fn fr_c2_node_write_refuses_an_existing_file_without_replace_and_replaces_with_it() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join(ENROL_FILE);
        fs::write(&path, b"old bytes").unwrap();
        let err = put(d.path(), &sample(), None, false).unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(fs::read(&path).unwrap(), b"old bytes");
        assert_eq!(names(d.path()), vec![ENROL_FILE.to_string()]);
        put(d.path(), &sample(), None, true).unwrap();
        assert_eq!(read_enrolment(d.path()).unwrap(), Some(sample()));
        assert_eq!(names(d.path()), vec![ENROL_FILE.to_string()], "no backup or temp file remains");
    }

    #[test]
    fn fr_c2_node_write_refuses_an_invalid_token_and_a_bad_peer_without_leftovers() {
        let d = tempfile::tempdir().unwrap();
        let mut e = sample();
        e.node_token = "has space".into();
        assert!(put(d.path(), &e, None, false).is_err());
        assert!(put(d.path(), &sample(), Some("nope"), false).is_err());
        assert!(names(d.path()).is_empty());
    }

    #[test]
    fn fr_c2_node_write_never_follows_a_planted_name_create_new() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("victim");
        fs::write(&target, b"victim").unwrap();
        std::os::unix::fs::symlink(&target, temp_name(d.path(), "enrol.json", "0123456789abcdef")).unwrap();
        let err = stage_file(d.path(), "enrol.json", "0123456789abcdef", b"new", 0o600).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&target).unwrap(), b"victim", "the symlink target is untouched");
        // A fresh suffix works, and a second call with the same one fails.
        stage_file(d.path(), "enrol.json", "fedcba9876543210", b"new", 0o600).unwrap();
        assert!(stage_file(d.path(), "enrol.json", "fedcba9876543210", b"again", 0o600).is_err());
    }

    #[test]
    fn fr_c2_node_write_rolls_back_when_the_second_rename_fails() {
        let d = tempfile::tempdir().unwrap();
        let old = peer();
        let new = peer();
        let enrol = d.path().join(ENROL_FILE);
        let peers = crate::transport::allowed_peers_file(d.path());
        fs::write(&enrol, b"previous enrol bytes").unwrap();
        fs::write(&peers, format!("{old}\n")).unwrap();
        let before = names(d.path());
        let calls = Cell::new(0);
        let failing = |from: &Path, to: &Path| -> io::Result<()> {
            calls.set(calls.get() + 1);
            if to.file_name().is_some_and(|n| n == "allowed-peers") {
                return Err(io::Error::other("injected"));
            }
            fs::rename(from, to)
        };
        let err = put_with(d.path(), &sample(), Some(&new), true, &failing).unwrap_err();
        assert!(format!("{err:#}").contains("allowed-peers"), "{err:#}");
        assert_eq!(calls.get(), 3, "enrol.json, allowed-peers, restore");
        assert_eq!(fs::read(&enrol).unwrap(), b"previous enrol bytes");
        assert_eq!(fs::read_to_string(&peers).unwrap(), format!("{old}\n"));
        assert_eq!(names(d.path()), before, "no temp or backup files remain");
    }

    #[test]
    fn fr_c2_node_write_rollback_removes_the_new_file_when_there_was_none() {
        let d = tempfile::tempdir().unwrap();
        let failing = |from: &Path, to: &Path| -> io::Result<()> {
            if to.file_name().is_some_and(|n| n == "allowed-peers") {
                return Err(io::Error::other("injected"));
            }
            fs::rename(from, to)
        };
        assert!(put_with(d.path(), &sample(), Some(&peer()), false, &failing).is_err());
        assert!(names(d.path()).is_empty(), "{:?}", names(d.path()));
    }

    #[test]
    fn fr_c2_node_write_rollback_when_the_first_rename_fails_keeps_the_previous_file() {
        let d = tempfile::tempdir().unwrap();
        let enrol = d.path().join(ENROL_FILE);
        fs::write(&enrol, b"previous enrol bytes").unwrap();
        let failing = |from: &Path, to: &Path| -> io::Result<()> {
            if from.to_string_lossy().ends_with(".tmp") {
                return Err(io::Error::other("injected"));
            }
            fs::rename(from, to)
        };
        assert!(put_with(d.path(), &sample(), None, true, &failing).is_err());
        assert_eq!(fs::read(&enrol).unwrap(), b"previous enrol bytes");
        assert_eq!(names(d.path()), vec![ENROL_FILE.to_string()]);
    }

    #[test]
    fn fr_c2_node_append_keeps_crlf_text_and_doubles_no_blank_line() {
        let a = peer();
        let b = peer();
        let existing = format!("# keep\r\n{a}\r\n");
        let out = append_allowed_peer(&existing, &b).unwrap().unwrap();
        assert_eq!(out, format!("{existing}{b}\n"));
        assert!(!out.contains("\n\n") && !out.contains("\r\n\r\n"), "no blank line is added");
        let out = append_allowed_peer(&format!("{a}\r"), &b).unwrap().unwrap();
        assert!(out.starts_with(&format!("{a}\r")) && out.ends_with(&format!("{b}\n")), "{out:?}");
        assert!(append_allowed_peer(&existing, &a).unwrap().is_none(), "a listed id parses out of CRLF text");
    }

    #[test]
    fn fr_c2_node_write_crlf_allowed_peers_keeps_old_bytes_and_appends() {
        let d = tempfile::tempdir().unwrap();
        let old = peer();
        let new = peer();
        let path = crate::transport::allowed_peers_file(d.path());
        let existing = format!("# keep\r\n{old}\r\n");
        fs::write(&path, &existing).unwrap();
        put(d.path(), &sample(), Some(&new), false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), format!("{existing}{new}\n"));
    }

    #[test]
    fn fr_c2_node_write_rollback_restores_bytes_and_mode_of_a_0640_previous_file() {
        let d = tempfile::tempdir().unwrap();
        let enrol = d.path().join(ENROL_FILE);
        fs::write(&enrol, b"previous enrol bytes").unwrap();
        fs::set_permissions(&enrol, fs::Permissions::from_mode(0o640)).unwrap();
        let before = names(d.path());
        let failing = |from: &Path, to: &Path| -> io::Result<()> {
            if to.file_name().is_some_and(|n| n == "allowed-peers") {
                return Err(io::Error::other("injected"));
            }
            fs::rename(from, to)
        };
        assert!(put_with(d.path(), &sample(), Some(&peer()), true, &failing).is_err());
        assert_eq!(fs::read(&enrol).unwrap(), b"previous enrol bytes");
        assert_eq!(mode(&enrol), 0o640);
        assert_eq!(names(d.path()), before, "no .bak or .tmp file remains");
    }

    #[test]
    fn fr_c2_node_write_a_failing_restore_says_enrol_json_holds_the_new_enrolment() {
        let d = tempfile::tempdir().unwrap();
        let enrol = d.path().join(ENROL_FILE);
        fs::write(&enrol, b"previous enrol bytes").unwrap();
        let calls = Cell::new(0);
        let failing = |from: &Path, to: &Path| -> io::Result<()> {
            calls.set(calls.get() + 1);
            if calls.get() >= 2 {
                return Err(io::Error::other("injected"));
            }
            fs::rename(from, to)
        };
        let err = put_with(d.path(), &sample(), Some(&peer()), true, &failing).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("now holds the NEW enrolment"), "{text}");
        assert!(!text.contains("tok_SECRET"), "{text}");
        assert_eq!(calls.get(), 3);
        assert_eq!(read_enrolment(d.path()).unwrap(), Some(sample()));
        assert_eq!(names(d.path()), vec![ENROL_FILE.to_string()], "no backup or temp file remains");
    }

    #[test]
    fn fr_c2_node_replace_never_lists_a_directory_without_enrol_json() {
        let d = tempfile::tempdir().unwrap();
        let enrol = d.path().join(ENROL_FILE);
        fs::write(&enrol, b"previous enrol bytes").unwrap();
        let seen = Cell::new(0);
        let checking = |from: &Path, to: &Path| -> io::Result<()> {
            seen.set(seen.get() + 1);
            assert!(enrol.exists(), "enrol.json must exist just before every rename");
            fs::rename(from, to)
        };
        put_with(d.path(), &sample(), Some(&peer()), true, &checking).unwrap();
        assert_eq!(seen.get(), 2);
        assert!(enrol.exists());
        // The same holds on the rollback path.
        let seen = Cell::new(0);
        let failing = |from: &Path, to: &Path| -> io::Result<()> {
            seen.set(seen.get() + 1);
            assert!(enrol.exists(), "enrol.json must exist just before every rename");
            if to.file_name().is_some_and(|n| n == "allowed-peers") {
                return Err(io::Error::other("injected"));
            }
            fs::rename(from, to)
        };
        assert!(put_with(d.path(), &sample(), Some(&peer()), true, &failing).is_err());
        assert_eq!(seen.get(), 3);
        assert!(enrol.exists());
    }

    #[test]
    fn fr_c2_node_read_refuses_a_symlinked_enrol_json_without_quoting_the_target() {
        let d = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let target = other.path().join("target.json");
        fs::write(&target, serde_json::to_vec(&sample()).unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, d.path().join(ENROL_FILE)).unwrap();
        let err = format!("{:#}", read_enrolment(d.path()).unwrap_err());
        assert!(err.contains("enrol.json") && err.contains("symbolic link"), "{err}");
        assert!(!err.contains("tok_SECRET"), "{err}");
    }

    #[test]
    fn fr_c2_node_read_of_a_fifo_named_enrol_json_fails_without_hanging() {
        use std::os::unix::ffi::OsStrExt;
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join(ENROL_FILE);
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let dir = d.path().to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(format!("{:#}", read_enrolment(&dir).unwrap_err()));
        });
        let err = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("read_enrolment hung on a FIFO");
        assert!(err.contains("not a regular file"), "{err}");
    }

    #[test]
    fn fr_c2_node_write_refused_when_nothing_changed() {
        let d = tempfile::tempdir().unwrap();
        let enrol = d.path().join(ENROL_FILE);
        fs::write(&enrol, b"old").unwrap();
        // An existing file without replace.
        let lock = lock_state_dir(d.path()).unwrap();
        let err = write_enrolment(&lock, &sample(), None, false).unwrap_err();
        assert!(matches!(err, WriteError::Refused(_)), "{err:?}");
        assert!(err.to_string().contains("already exists"), "{err}");
        drop(lock);
        // The first rename failing, with nothing moved.
        let failing = |_: &Path, _: &Path| -> io::Result<()> { Err(io::Error::other("injected")) };
        let err = put_with(d.path(), &sample(), None, true, &failing).unwrap_err();
        assert!(matches!(err, WriteError::Refused(_)), "{err:?}");
        // An invalid token.
        let mut bad = sample();
        bad.node_token = "has space".into();
        let err = put_with(d.path(), &bad, None, true, &|f, t| fs::rename(f, t)).unwrap_err();
        assert!(matches!(err, WriteError::Refused(_)), "{err:?}");
        assert_eq!(fs::read(&enrol).unwrap(), b"old");
        assert_eq!(names(d.path()), vec![ENROL_FILE.to_string()]);
    }

    #[test]
    fn fr_c2_node_write_symlinked_allowed_peers_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let target = other.path().join("real-peers");
        fs::write(&target, format!("{}\n", peer())).unwrap();
        std::os::unix::fs::symlink(&target, crate::transport::allowed_peers_file(d.path())).unwrap();
        let lock = lock_state_dir(d.path()).unwrap();
        let err = write_enrolment(&lock, &sample(), Some(&peer()), false).unwrap_err();
        assert!(matches!(err, WriteError::Refused(_)), "{err:?}");
        assert!(!d.path().join(ENROL_FILE).exists());
    }

    #[test]
    fn fr_c2_node_write_second_rename_failing_with_a_good_undo_is_rolled_back() {
        let d = tempfile::tempdir().unwrap();
        let enrol = d.path().join(ENROL_FILE);
        fs::write(&enrol, b"previous enrol bytes").unwrap();
        let failing = |from: &Path, to: &Path| -> io::Result<()> {
            if to.file_name().is_some_and(|n| n == "allowed-peers") {
                return Err(io::Error::other("injected"));
            }
            fs::rename(from, to)
        };
        let err = put_with(d.path(), &sample(), Some(&peer()), true, &failing).unwrap_err();
        assert!(matches!(err, WriteError::RolledBack(_)), "{err:?}");
        assert!(err.to_string().contains("allowed-peers"), "{err}");
        assert_eq!(fs::read(&enrol).unwrap(), b"previous enrol bytes");
    }

    #[test]
    fn fr_c2_node_write_a_failing_undo_is_new_kept() {
        let d = tempfile::tempdir().unwrap();
        let enrol = d.path().join(ENROL_FILE);
        fs::write(&enrol, b"previous enrol bytes").unwrap();
        let calls = Cell::new(0);
        let failing = |from: &Path, to: &Path| -> io::Result<()> {
            calls.set(calls.get() + 1);
            if calls.get() >= 2 {
                return Err(io::Error::other("injected"));
            }
            fs::rename(from, to)
        };
        let err = put_with(d.path(), &sample(), Some(&peer()), true, &failing).unwrap_err();
        assert!(matches!(err, WriteError::NewKept(_)), "{err:?}");
        assert!(err.to_string().contains("now holds the NEW enrolment"), "{err}");
        assert_eq!(read_enrolment(d.path()).unwrap(), Some(sample()));
    }

    #[test]
    fn fr_c2_node_write_a_failing_directory_sync_is_not_synced_and_keeps_both_files() {
        let d = tempfile::tempdir().unwrap();
        let new = peer();
        let failing_sync = |_: &Path| -> io::Result<()> { Err(io::Error::other("injected sync")) };
        let err = put_sync(d.path(), &sample(), Some(&new), &failing_sync).unwrap_err();
        assert!(matches!(&err, WriteError::NotSynced(_, r) if r.peer_added), "{err:?}");
        assert!(err.to_string().contains("the files are kept"), "{err}");
        assert_eq!(read_enrolment(d.path()).unwrap(), Some(sample()), "enrol.json holds the new enrolment");
        assert_eq!(
            fs::read_to_string(crate::transport::allowed_peers_file(d.path())).unwrap(),
            format!("{new}\n"),
            "allowed-peers holds the new peer"
        );
        assert_eq!(names(d.path()), vec!["allowed-peers".to_string(), ENROL_FILE.to_string()], "no temp file remains");
    }

    #[test]
    fn fr_c2_node_write_error_display_is_the_inner_text_and_converts_into_anyhow() {
        let e = WriteError::Refused(anyhow::anyhow!("inner").context("outer"));
        assert_eq!(e.to_string(), "outer: inner");
        assert_eq!(format!("{e:#}"), "outer: inner");
        let any: anyhow::Error = e.into();
        assert_eq!(format!("{any:#}"), "outer: inner", "no repeated source chain");
    }

    #[test]
    fn fr_c2_node_write_report_says_whether_a_new_line_was_added() {
        let d = tempfile::tempdir().unwrap();
        let a = peer();
        // No peer asked for.
        assert!(!put(d.path(), &sample(), None, false).unwrap().peer_added);
        // A new peer.
        assert!(put(d.path(), &sample(), Some(&a), true).unwrap().peer_added);
        // The same peer again: already listed.
        assert!(!put(d.path(), &sample(), Some(&a), true).unwrap().peer_added);
        assert_eq!(fs::read_to_string(crate::transport::allowed_peers_file(d.path())).unwrap(), format!("{a}\n"));
    }

    #[test]
    fn fr_c2_node_write_refuses_an_oversized_allowed_peers() {
        let d = tempfile::tempdir().unwrap();
        let path = crate::transport::allowed_peers_file(d.path());
        fs::write(&path, vec![b'#'; (MAX_FILE_BYTES + 1) as usize]).unwrap();
        let lock = lock_state_dir(d.path()).unwrap();
        let err = write_enrolment(&lock, &sample(), Some(&peer()), false).unwrap_err();
        assert!(matches!(err, WriteError::Refused(_)), "{err:?}");
        assert!(err.to_string().contains("larger than"), "{err}");
        assert!(!d.path().join(ENROL_FILE).exists());
    }

    #[test]
    fn fr_c2_node_lock_file_must_be_a_regular_file_owned_by_this_user() {
        let p = Path::new("/x/.enrol.lock");
        assert!(check_lock_file(true, euid(), p).is_ok());
        let err = check_lock_file(false, euid(), p).unwrap_err().to_string();
        assert!(err.contains(".enrol.lock") && err.contains("not a regular file"), "{err}");
        let err = check_lock_file(true, euid().wrapping_add(1), p).unwrap_err().to_string();
        assert!(err.contains(".enrol.lock") && err.contains("not owned by this user"), "{err}");
    }

    // A file owned by another user cannot be made portably without root: the "is not owned by this
    // user" branch of open_regular_owned has no test here.

    #[test]
    fn fr_c2_node_write_refuses_a_symlinked_allowed_peers_and_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let target = other.path().join("real-peers");
        let old = peer();
        fs::write(&target, format!("{old}\n")).unwrap();
        std::os::unix::fs::symlink(&target, crate::transport::allowed_peers_file(d.path())).unwrap();
        let err = format!("{:#}", put(d.path(), &sample(), Some(&peer()), false).unwrap_err());
        assert!(err.contains("allowed-peers") && err.contains("symbolic link"), "{err}");
        assert_eq!(fs::read_to_string(&target).unwrap(), format!("{old}\n"), "the target is unchanged");
        assert!(fs::symlink_metadata(crate::transport::allowed_peers_file(d.path())).unwrap().file_type().is_symlink());
        assert!(!d.path().join(ENROL_FILE).exists());
        assert_eq!(names(d.path()), vec!["allowed-peers".to_string()], "no temp file remains");
    }

    #[test]
    fn fr_c2_node_lock_is_exclusive_per_state_dir_and_released_on_drop() {
        let d = tempfile::tempdir().unwrap();
        let first = lock_state_dir(d.path()).unwrap();
        assert_eq!(first.dir(), d.path());
        let err = lock_state_dir(d.path()).unwrap_err().to_string();
        assert!(err.contains("another ember-node enrol is running for this state directory"), "{err}");
        drop(first);
        lock_state_dir(d.path()).unwrap();
    }

    #[test]
    fn fr_c2_node_lock_creates_a_missing_dir_0700_and_leaves_an_existing_mode_alone() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("x").join("node");
        drop(lock_state_dir(&dir).unwrap());
        assert_eq!(mode(&dir), 0o700);
        let existing = d.path().join("shared");
        fs::create_dir(&existing).unwrap();
        fs::set_permissions(&existing, fs::Permissions::from_mode(0o755)).unwrap();
        drop(lock_state_dir(&existing).unwrap());
        assert_eq!(mode(&existing), 0o755, "an existing dir is not chmodded");
    }

    // ---- preflight (N2b): every refusal comes before any network ------------------------------

    fn locked(d: &Path) -> EnrolLock {
        lock_state_dir(d).unwrap()
    }

    #[test]
    fn fr_c2_node_preflight_passes_on_a_clean_dir_and_leaves_no_file() {
        let d = tempfile::tempdir().unwrap();
        let lock = locked(d.path());
        preflight(&lock, false).unwrap();
        preflight(&lock, true).unwrap();
        assert!(names(d.path()).is_empty(), "{:?}", names(d.path()));
    }

    #[test]
    fn fr_c2_node_preflight_passes_with_a_good_enrol_json_and_allowed_peers() {
        let d = tempfile::tempdir().unwrap();
        put(d.path(), &sample(), None, false).unwrap();
        fs::write(crate::transport::allowed_peers_file(d.path()), format!("# mine\n{}\n", peer())).unwrap();
        let lock = locked(d.path());
        preflight(&lock, true).unwrap();
        assert_eq!(names(d.path()), vec!["allowed-peers".to_string(), ENROL_FILE.to_string()]);
    }

    #[test]
    fn fr_c2_node_preflight_replace_refuses_an_unreadable_enrol_json() {
        // A symlink.
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("elsewhere");
        fs::write(&target, b"{}").unwrap();
        std::os::unix::fs::symlink(&target, d.path().join(ENROL_FILE)).unwrap();
        let lock = locked(d.path());
        let err = preflight(&lock, true).unwrap_err().to_string();
        assert!(err.contains("symbolic link") && err.contains("enrol again"), "{err}");
        // Without replace the plain "exists" refusal is the caller's: preflight does not look.
        preflight(&lock, false).unwrap();
        drop(lock);
        // A directory (not a regular file).
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join(ENROL_FILE)).unwrap();
        let lock = locked(d.path());
        let err = preflight(&lock, true).unwrap_err().to_string();
        assert!(err.contains("not a regular file") || err.contains("cannot open"), "{err}");
        drop(lock);
        // Over 64 KiB.
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join(ENROL_FILE), vec![b' '; (MAX_FILE_BYTES + 1) as usize]).unwrap();
        let lock = locked(d.path());
        let err = preflight(&lock, true).unwrap_err().to_string();
        assert!(err.contains("larger than"), "{err}");
        assert!(!names(d.path()).iter().any(|n| n.contains("preflight")));
    }

    #[test]
    fn fr_c2_node_preflight_refuses_a_bad_allowed_peers() {
        let cases: Vec<(&str, Box<dyn Fn(&Path)>, &str)> = vec![
            (
                "symlink",
                Box::new(|d: &Path| {
                    let t = d.join("real");
                    fs::write(&t, b"").unwrap();
                    std::os::unix::fs::symlink(&t, crate::transport::allowed_peers_file(d)).unwrap();
                }),
                "symbolic link",
            ),
            (
                "dangling symlink",
                Box::new(|d: &Path| {
                    std::os::unix::fs::symlink(d.join("nowhere"), crate::transport::allowed_peers_file(d)).unwrap();
                }),
                "symbolic link",
            ),
            (
                "directory",
                Box::new(|d: &Path| fs::create_dir(crate::transport::allowed_peers_file(d)).unwrap()),
                "not a regular file",
            ),
            (
                "too big",
                Box::new(|d: &Path| {
                    fs::write(crate::transport::allowed_peers_file(d), vec![b'#'; (MAX_FILE_BYTES + 1) as usize]).unwrap()
                }),
                "larger than",
            ),
            (
                "not utf-8",
                Box::new(|d: &Path| fs::write(crate::transport::allowed_peers_file(d), [0xffu8, 0xfe, b'\n']).unwrap()),
                "UTF-8",
            ),
            (
                "does not parse",
                Box::new(|d: &Path| fs::write(crate::transport::allowed_peers_file(d), b"not-a-key\n").unwrap()),
                "does not parse",
            ),
        ];
        for (label, plant, want) in cases {
            let d = tempfile::tempdir().unwrap();
            plant(d.path());
            let lock = locked(d.path());
            let err = preflight(&lock, false).unwrap_err().to_string();
            assert!(err.contains(want), "{label}: {err}");
            assert!(err.contains("nothing was redeemed"), "{label}: says what to do: {err}");
            assert!(!names(d.path()).iter().any(|n| n.contains("preflight")), "{label}: no probe file left");
        }
    }

    #[test]
    fn fr_c2_node_preflight_refuses_a_state_dir_that_is_not_writable() {
        if euid() == 0 {
            return; // root can write anywhere
        }
        let d = tempfile::tempdir().unwrap();
        let lock = locked(d.path());
        fs::set_permissions(d.path(), fs::Permissions::from_mode(0o500)).unwrap();
        let err = preflight(&lock, false).unwrap_err().to_string();
        fs::set_permissions(d.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(err.contains("not writable"), "{err}");
        assert!(!names(d.path()).iter().any(|n| n.contains("preflight")));
    }

    /// `preflight` and `prepare` use the same readers: whatever the one refuses, the other does.
    #[test]
    fn fr_c2_node_preflight_and_write_refuse_the_same_allowed_peers() {
        let d = tempfile::tempdir().unwrap();
        fs::write(crate::transport::allowed_peers_file(d.path()), b"garbage\n").unwrap();
        let lock = locked(d.path());
        assert!(preflight(&lock, false).is_err());
        let mut e = sample();
        e.server_peer_id = Some(peer());
        let id = e.server_peer_id.clone().unwrap();
        assert!(matches!(write_enrolment(&lock, &e, Some(&id), false), Err(WriteError::Refused(_))));
    }
}
