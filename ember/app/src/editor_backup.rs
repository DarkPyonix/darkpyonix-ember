//! Hot exit of the Ember 에디터 window (SPEC `FR-L31.6a`, `INTENT.md` D43, #744): the unsaved
//! buffers of a window are kept in a backup file on this device and put back when the window opens
//! again for the same computer, project and folder.
//!
//! - The file format, its location, pruning and the atomic write are pure functions.
//! - [`Tracker`] is the core without a clock or a window: it follows the documents the window shows
//!   ([`Tracker::observe`]), says what to put back ([`Tracker::take_restore_plan`]) and writes the
//!   file when something changed ([`Tracker::flush`]).
//! - [`view`] is the small part the window calls: it feeds the tracker, sends the restore commands
//!   and starts the debounced write. [`flush_on_exit`] is called on the exit path of `FR-L31.1`.
//!
//! As in Code-OSS, the backup is written while the window is open, not only on close: the toolkit
//! gives the editor's single window no close-request event (the `Window` element has one, but the
//! editor is the application's root, and no application-will-terminate hook exists), and a quit ends
//! the process inside its run loop. So the write follows the edit closely ([`WriteSchedule`]): a
//! short pause after the last change, and never later than [`BACKUP_DEBOUNCE_MS`] after the first
//! change that is not in the file yet, so continuous typing cannot push the write out.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::ui::editor_states::{reopen_plan, Reopen};

/// The format this build writes and reads. A file of another version is not read.
pub const BACKUP_VERSION: u32 = 1;
/// How long a backup is kept when the prefs say nothing (`Prefs::hot_exit_backup_days`).
pub const DEFAULT_BACKUP_DAYS: u32 = 30;
/// The longest the backup file may be behind the window: the write is due this long after the first
/// change that is not in the file, however long the typing goes on.
pub const BACKUP_DEBOUNCE_MS: u64 = 1000;
/// The pause after the last change before the backup is written (a burst of keys is one write).
pub const BACKUP_QUIET_MS: u64 = 200;

/// When the next backup write is due. A pure clock-free core: callers pass `now_ms`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteSchedule {
    /// When the first change not yet written happened.
    first: Option<u64>,
    last: u64,
    /// A task is waiting for the write to come due.
    running: bool,
}

impl WriteSchedule {
    /// A change happened. True when the caller has to start the waiting task (none is running).
    pub fn note_change(&mut self, now_ms: u64) -> bool {
        self.first.get_or_insert(now_ms);
        self.last = now_ms;
        !std::mem::replace(&mut self.running, true)
    }

    /// Milliseconds until the write is due, `Some(0)` when it is due now, `None` when nothing is
    /// waiting (the task ends, and is started again by the next change).
    pub fn due_in(&mut self, now_ms: u64) -> Option<u64> {
        let Some(first) = self.first else {
            self.running = false;
            return None;
        };
        let due = (self.last + BACKUP_QUIET_MS).min(first + BACKUP_DEBOUNCE_MS);
        Some(due.saturating_sub(now_ms))
    }

    /// The write is starting: what it covers is no longer waiting. A change during the write is a new wait.
    pub fn written(&mut self) {
        self.first = None;
    }
}

const DAY_MS: u64 = 86_400_000;

/// What one backup file belongs to: a folder of a project on a computer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BackupKey {
    pub computer: String,
    pub project: String,
    pub folder: String,
}

impl BackupKey {
    pub fn new(computer: &str, project: &str, folder: &str) -> Self {
        Self { computer: computer.to_owned(), project: project.to_owned(), folder: folder.to_owned() }
    }
}

/// FNV-1a, 64 bit. Stable across builds and platforms (the standard hasher is not promised to be).
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// A mark of the disk text a buffer was edited from: its length and a hash. Only equality is asked
/// of it (did the file change since?), so the text itself is not stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseMark {
    pub len: u64,
    /// FNV-1a 64 of the text, 16 hex digits.
    pub hash: String,
}

impl BaseMark {
    pub fn of(text: &str) -> Self {
        Self { len: text.len() as u64, hash: format!("{:016x}", fnv1a64(text.as_bytes())) }
    }
}

/// One unsaved buffer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupEntry {
    pub path: String,
    /// The buffer, with `\n` line breaks as the window holds it.
    pub text: String,
    /// The disk text the edits started from; `None` when the window never saw the file clean.
    #[serde(default)]
    pub base: Option<BaseMark>,
}

/// The backup file of one window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupFile {
    pub version: u32,
    /// When it was written, in milliseconds since the Unix epoch.
    pub written_at_ms: u64,
    pub computer: String,
    pub project: String,
    pub folder: String,
    /// The tab that was current, when it is one of the entries.
    #[serde(default)]
    pub current: Option<String>,
    pub entries: Vec<BackupEntry>,
}

/// Why a backup file was not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupError {
    /// Not JSON, or not the fields of this format.
    Corrupt(String),
    /// Written by another format version.
    Version(u64),
}

impl std::fmt::Display for BackupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackupError::Corrupt(reason) => write!(f, "not a backup file: {reason}"),
            BackupError::Version(version) => write!(f, "backup format {version}, this build reads {BACKUP_VERSION}"),
        }
    }
}

pub fn encode(file: &BackupFile) -> Vec<u8> {
    // A struct of strings and numbers always serializes.
    serde_json::to_vec(file).unwrap_or_default()
}

/// Reads a backup file. The version is checked before the fields, so a newer format with other
/// fields reads as [`BackupError::Version`].
pub fn decode(bytes: &[u8]) -> Result<BackupFile, BackupError> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| BackupError::Corrupt(e.to_string()))?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| BackupError::Corrupt("no version".to_owned()))?;
    if version != u64::from(BACKUP_VERSION) {
        return Err(BackupError::Version(version));
    }
    serde_json::from_value(value).map_err(|e| BackupError::Corrupt(e.to_string()))
}

/// The folder of the backup files, beside the prefs file (`<data folder>/editor/Backups`).
pub fn backup_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("editor").join("Backups")
}

/// The file name of a window's backup: the project's name for a person reading the folder, and a
/// hash of computer, project and folder so each of them has its own file.
pub fn backup_file_name(key: &BackupKey) -> String {
    format!("{}.json", backup_stem(key))
}

/// The part of every backup file name that belongs to one (computer, project, folder).
fn backup_stem(key: &BackupKey) -> String {
    let id = format!("{}\u{0}{}\u{0}{}", key.computer, key.project, key.folder);
    format!("{}-{:016x}", crate::config::safe_file_stem(&key.project), fnv1a64(id.as_bytes()))
}

/// The file name of one editor process's backup. Two windows on the same folder are two processes
/// and must not write one file: the one that quit last would leave only its own buffers behind.
pub fn window_file_name(key: &BackupKey, pid: u32) -> String {
    format!("{}.{pid}.json", backup_stem(key))
}

/// The name of the file a running window touches to say it is alive.
pub fn live_marker_name(key: &BackupKey, pid: u32) -> String {
    format!("{}.{pid}.live", backup_stem(key))
}

/// How long ago a window's marker may have been touched for the window to count as running.
pub const LIVE_WINDOW_MS: u64 = 20_000;
/// How often a running window touches its marker.
pub const HEARTBEAT_MS: u64 = 5_000;

/// Whether the window that owns `marker` is running: its marker was touched within [`LIVE_WINDOW_MS`].
pub fn marker_is_live(marker: &Path, now: std::time::SystemTime) -> bool {
    let Ok(modified) = std::fs::metadata(marker).and_then(|m| m.modified()) else { return false };
    now.duration_since(modified).map(|age| age.as_millis() as u64 <= LIVE_WINDOW_MS).unwrap_or(true)
}

/// Removes the markers in `dir` nobody touched for a day: windows that were killed.
pub fn prune_markers(dir: &Path, now: std::time::SystemTime) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for path in read.flatten().map(|e| e.path()) {
        if path.extension().and_then(|e| e.to_str()) != Some("live") {
            continue;
        }
        let old = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age.as_millis() as u64 > DAY_MS));
        if old {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Marks this window as running (creates or touches its marker).
pub fn touch_marker(dir: &Path, key: &BackupKey, pid: u32) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(live_marker_name(key, pid)), b"")
}

/// Whether a backup written at `written_at_ms` is older than `days`.
pub fn expired(written_at_ms: u64, now_ms: u64, days: u32) -> bool {
    now_ms.saturating_sub(written_at_ms) > u64::from(days) * DAY_MS
}

/// Writes `bytes` to a temporary file beside `path` and renames it over `path`. On Unix the file
/// is readable by its owner only (0600): it holds unsaved source text.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        // The mode above applies to a new file only; a temporary file left by a killed write keeps its own.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// What [`load`] found at a window's backup path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loaded {
    /// No backup.
    None,
    Restored(BackupFile),
    /// Older than the limit: removed.
    Expired,
    /// The file is there but was not read. `kept_as` is where it was moved so the next write does
    /// not replace it.
    Unreadable { reason: String, kept_as: Option<PathBuf> },
}

/// Reads the backup at `path` for `key`. A file that does not parse (or has another version) is
/// renamed to `<path>.corrupt` and reported; an expired one is removed. Never fails: the window
/// opens whatever the file is.
pub fn load(path: &Path, key: &BackupKey, now_ms: u64, days: u32) -> Loaded {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::None,
        Err(e) => return Loaded::Unreadable { reason: e.to_string(), kept_as: None },
    };
    let file = match decode(&bytes) {
        Ok(file) => file,
        Err(error) => {
            let mut aside = path.as_os_str().to_owned();
            aside.push(".corrupt");
            let aside = PathBuf::from(aside);
            let kept_as = std::fs::rename(path, &aside).ok().map(|_| aside);
            return Loaded::Unreadable { reason: error.to_string(), kept_as };
        }
    };
    if expired(file.written_at_ms, now_ms, days) {
        let _ = std::fs::remove_file(path);
        return Loaded::Expired;
    }
    // Two keys with one file name (a hash collision): the file is another window's.
    if file.computer != key.computer || file.project != key.project || file.folder != key.folder {
        return Loaded::None;
    }
    Loaded::Restored(file)
}

/// The backups other (finished) windows left for one folder, put together for a window that opens.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Collected {
    /// Every buffer of those files; a path in more than one keeps its newest text.
    pub merged: Option<BackupFile>,
    /// The files `merged` was made from. They are removed once `merged` is in the opening window's own file.
    pub sources: Vec<PathBuf>,
    /// Files that were not read, as [`load`] reports them.
    pub unreadable: Vec<(String, Option<PathBuf>)>,
}

/// Puts several backups of one folder into one, the newest first: its tab is the current one, and a
/// path that is in more than one keeps the newest file's text.
pub fn merge_backups(mut files: Vec<BackupFile>) -> Option<BackupFile> {
    files.sort_by(|a, b| b.written_at_ms.cmp(&a.written_at_ms));
    let mut files = files.into_iter();
    let mut merged = files.next()?;
    for next in files {
        for entry in next.entries {
            if !merged.entries.iter().any(|e| e.path == entry.path) {
                merged.entries.push(entry);
            }
        }
    }
    Some(merged)
}

/// Reads the backups of the windows that are not running for `key`: every `<stem>.<pid>.json`
/// whose owner `is_live` does not name (a process ends without a final write when it is killed),
/// and the `<stem>.json` older builds wrote. A running window's file is left alone; it is that
/// window's. `own_pid` is this window: a file under its number is left over from a process that
/// had the number before.
pub fn collect_for_key(dir: &Path, key: &BackupKey, own_pid: u32, now_ms: u64, days: u32, is_live: impl Fn(u32) -> bool) -> Collected {
    let stem = backup_stem(key);
    let mut out = Collected::default();
    let Ok(read) = std::fs::read_dir(dir) else { return out };
    let mut paths: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
    paths.sort();
    let mut files = Vec::new();
    for path in paths {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let Some(rest) = name.strip_prefix(stem.as_str()).and_then(|r| r.strip_suffix(".json")) else { continue };
        let owner = match rest {
            "" => None,
            r => match r.strip_prefix('.').and_then(|p| p.parse::<u32>().ok()) {
                Some(pid) => Some(pid),
                None => continue,
            },
        };
        if owner.is_some_and(|pid| pid != own_pid && is_live(pid)) {
            continue;
        }
        match load(&path, key, now_ms, days) {
            Loaded::Restored(file) => {
                out.sources.push(path);
                files.push(file);
            }
            Loaded::Unreadable { reason, kept_as } => out.unreadable.push((reason, kept_as)),
            Loaded::None | Loaded::Expired => {}
        }
    }
    out.merged = merge_backups(files);
    out
}

/// Removes every backup in `dir` older than `days`, whichever project it belongs to, and returns
/// the removed paths. Files that do not parse are left for [`load`] to report.
pub fn prune_dir(dir: &Path, now_ms: u64, days: u32) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return removed };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let Ok(file) = decode(&bytes) else { continue };
        if expired(file.written_at_ms, now_ms, days) && std::fs::remove_file(&path).is_ok() {
            removed.push(path);
        }
    }
    removed.sort();
    removed
}

/// A document as the window shows it, for [`Tracker::observe`].
#[derive(Debug, Clone, Copy)]
pub struct DocView<'a> {
    pub path: &'a str,
    pub generation: u64,
    pub version: u32,
    pub dirty: bool,
    pub text: &'a str,
}

/// Which state of a document was copied: the node generation, the version and the length.
type Stamp = (u64, u32, usize);

fn stamp(doc: &DocView<'_>) -> Stamp {
    (doc.generation, doc.version, doc.text.len())
}

#[derive(Debug)]
struct Tracked {
    path: String,
    text: String,
    base: Option<BaseMark>,
    stamp: Stamp,
    /// Put back from the backup and not yet reported dirty by the session: kept while clean.
    awaiting_dirty: bool,
    /// The session is opening it again after a reconnect: kept while its document shows the disk
    /// text, until the buffer is back in it.
    reopening: bool,
}

#[derive(Debug)]
struct Restoring {
    entry: BackupEntry,
    /// The file on disk is not the one the buffer was edited from.
    conflict: bool,
    /// The file on disk equals the buffer: nothing unsaved is left.
    equals_disk: bool,
    /// The file could not be opened; the entry stays in the backup.
    failed: bool,
}

/// What [`Tracker::observe`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    /// The backup on disk is no longer what the window holds: a write is due.
    pub changed: bool,
    /// Restored buffers whose file changed on disk since the backup: the window shows the bar of
    /// `FR-L31.6` (2) for each, once.
    pub conflicts: Vec<String>,
}

/// What [`Tracker::flush`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Flush {
    Unchanged,
    Written(PathBuf),
    /// No unsaved buffer is left: the file is gone.
    Removed(PathBuf),
}

/// The unsaved buffers of one window and the state of its backup file.
#[derive(Debug)]
pub struct Tracker {
    key: BackupKey,
    /// Entries of the backup read at open that are not in a document yet. They stay in the file.
    restoring: Vec<Restoring>,
    current: Option<String>,
    planned: bool,
    tracked: Vec<Tracked>,
    /// The mark of each open document when it was last clean.
    clean: HashMap<String, (Stamp, BaseMark)>,
    seq: u64,
    written: u64,
    /// The file this window writes in the backup folder.
    file_name: String,
}

impl Tracker {
    /// A tracker for the window of `key`, with the backup read at open, if any.
    pub fn new(key: BackupKey, loaded: Option<BackupFile>) -> Self {
        let (restoring, current) = match loaded {
            Some(file) => (
                file.entries.into_iter().map(|entry| Restoring { entry, conflict: false, equals_disk: false, failed: false }).collect(),
                file.current,
            ),
            None => (Vec::new(), None),
        };
        let file_name = backup_file_name(&key);
        Self { key, restoring, current, planned: false, tracked: Vec::new(), clean: HashMap::new(), seq: 0, written: 0, file_name }
    }

    /// Writes this window's backup under `name` instead of the shared name of older builds.
    pub fn with_file_name(mut self, name: String) -> Self {
        self.file_name = name;
        self
    }

    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    /// Asks the next flush to write even though nothing changed, for a window that took over the
    /// buffers of other windows' files and has to own them before those files go.
    pub fn mark_changed(&mut self) {
        self.seq += 1;
    }

    pub fn key(&self) -> &BackupKey {
        &self.key
    }

    /// The number of the newest change; a debounced write carries it and writes only if it still is.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// The steps that put the backup's buffers back, once: each restored in its tab, in the
    /// backup's order, then the backup's current tab (or the last one) focused. Empty afterwards.
    pub fn take_restore_plan(&mut self) -> Vec<Reopen> {
        if std::mem::replace(&mut self.planned, true) {
            return Vec::new();
        }
        let tabs: Vec<(String, Option<String>)> = self.restoring.iter().map(|r| (r.entry.path.clone(), Some(r.entry.text.clone()))).collect();
        let current = self
            .current
            .clone()
            .filter(|c| tabs.iter().any(|(p, _)| p == c))
            .or_else(|| tabs.last().map(|(p, _)| p.clone()));
        reopen_plan(&tabs, current.as_deref())
    }

    /// The session opened `path` and read `disk_text` from the disk. For a buffer being put back
    /// this decides whether its file changed since the backup.
    pub fn opened(&mut self, path: &str, disk_text: &str) {
        if let Some(r) = self.restoring.iter_mut().find(|r| r.entry.path == path) {
            r.failed = false;
            r.equals_disk = disk_text == r.entry.text;
            r.conflict = !r.equals_disk && r.entry.base.as_ref().is_some_and(|base| *base != BaseMark::of(disk_text));
        }
        // After a reconnect the disk already has the buffer's text: the document stays clean.
        if let Some(t) = self.tracked.iter_mut().find(|t| t.path == path) {
            if t.text == disk_text {
                t.reopening = false;
            }
        }
    }

    /// The session reconnected and the window puts its buffers back (`FR-L31.6`): each document shows
    /// the disk text for a moment. The unsaved buffers stay in the backup through it, so a write in
    /// that moment (the exit path) does not drop them.
    pub fn reconnected(&mut self) {
        for t in &mut self.tracked {
            t.reopening = true;
        }
    }

    /// The session could not open `path`. True when it is a buffer being put back, the first time:
    /// its entry stays in the backup and the window says so.
    pub fn open_failed(&mut self, path: &str) -> bool {
        if let Some(t) = self.tracked.iter_mut().find(|t| t.path == path) {
            t.reopening = false;
        }
        match self.restoring.iter_mut().find(|r| r.entry.path == path) {
            Some(r) => !std::mem::replace(&mut r.failed, true),
            None => false,
        }
    }

    /// Follows the documents the window shows, in tab order, after the session's updates were
    /// applied. A dirty document is in the backup with its text; a clean or closed one is not.
    pub fn observe(&mut self, docs: &[DocView<'_>], current: Option<&str>) -> Observed {
        let mut out = Observed::default();
        // A buffer being put back is restored once its document shows the backup's text (or the
        // person already typed in it).
        let mut adopted: Vec<Tracked> = Vec::new();
        self.restoring.retain(|r| {
            let Some(doc) = docs.iter().find(|d| d.path == r.entry.path) else { return true };
            let shown = doc.text == r.entry.text;
            if !shown && !doc.dirty {
                return true;
            }
            if shown && r.equals_disk {
                // The disk caught up with the buffer: nothing is unsaved.
                out.changed = true;
                return false;
            }
            if r.conflict {
                out.conflicts.push(r.entry.path.clone());
            }
            adopted.push(Tracked {
                path: r.entry.path.clone(),
                text: doc.text.to_owned(),
                base: r.entry.base.clone(),
                stamp: stamp(doc),
                awaiting_dirty: !doc.dirty,
                reopening: false,
            });
            if !shown {
                out.changed = true;
            }
            false
        });
        self.tracked.extend(adopted);

        for doc in docs {
            if self.restoring.iter().any(|r| r.entry.path == doc.path) {
                continue;
            }
            let now = stamp(doc);
            let at = self.tracked.iter().position(|t| t.path == doc.path);
            if doc.dirty {
                match at {
                    Some(i) => {
                        let tracked = &mut self.tracked[i];
                        tracked.awaiting_dirty = false;
                        tracked.reopening = false;
                        if tracked.stamp != now {
                            tracked.text = doc.text.to_owned();
                            tracked.stamp = now;
                            out.changed = true;
                        }
                    }
                    None => {
                        let base = self.clean.get(doc.path).map(|(_, mark)| mark.clone());
                        self.tracked.push(Tracked { path: doc.path.to_owned(), text: doc.text.to_owned(), base, stamp: now, awaiting_dirty: false, reopening: false });
                        out.changed = true;
                    }
                }
                continue;
            }
            if let Some(i) = at {
                // Just put back and the session's dirty mark has not arrived: still unsaved.
                if self.tracked[i].awaiting_dirty && self.tracked[i].stamp == now {
                    continue;
                }
                // Opened again after a reconnect and the buffer is not back in yet: still unsaved.
                if self.tracked[i].reopening {
                    continue;
                }
                // Saved, or reverted to the disk version.
                self.tracked.remove(i);
                out.changed = true;
            }
            let known = self.clean.get(doc.path).is_some_and(|(seen, _)| *seen == now);
            if !known {
                self.clean.insert(doc.path.to_owned(), (now, BaseMark::of(doc.text)));
            }
        }

        // A closed document (저장 안 함, a deleted file's tab) leaves the backup.
        let before = self.tracked.len();
        self.tracked.retain(|t| docs.iter().any(|d| d.path == t.path));
        if self.tracked.len() != before {
            out.changed = true;
        }
        self.clean.retain(|path, _| docs.iter().any(|d| d.path == path.as_str()));

        if let Some(current) = current {
            self.current = Some(current.to_owned());
        }
        if out.changed {
            self.seq += 1;
        }
        out
    }

    /// The backup file for what the window holds now; `None` when nothing is unsaved.
    pub fn snapshot(&self, now_ms: u64) -> Option<BackupFile> {
        let mut entries: Vec<BackupEntry> = self.restoring.iter().map(|r| r.entry.clone()).collect();
        entries.extend(self.tracked.iter().map(|t| BackupEntry { path: t.path.clone(), text: t.text.clone(), base: t.base.clone() }));
        if entries.is_empty() {
            return None;
        }
        let current = self.current.clone().filter(|c| entries.iter().any(|e| e.path == *c));
        Some(BackupFile {
            version: BACKUP_VERSION,
            written_at_ms: now_ms,
            computer: self.key.computer.clone(),
            project: self.key.project.clone(),
            folder: self.key.folder.clone(),
            current,
            entries,
        })
    }

    /// Writes the backup into `dir` if something changed since the last write, or removes it when
    /// nothing is unsaved. A failed write is tried again by the next flush.
    pub fn flush(&mut self, dir: &Path, now_ms: u64) -> std::io::Result<Flush> {
        if self.written == self.seq {
            return Ok(Flush::Unchanged);
        }
        let path = dir.join(&self.file_name);
        let done = match self.snapshot(now_ms) {
            Some(file) => {
                write_atomic(&path, &encode(&file))?;
                Flush::Written(path)
            }
            None => {
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
                Flush::Removed(path)
            }
        };
        self.written = self.seq;
        Ok(done)
    }
}

/// The notice for a backup file that was not read.
pub fn unreadable_notice(kept_as: Option<&Path>) -> String {
    match kept_as {
        Some(path) => format!(
            "저장하지 않은 변경 내용의 백업 파일을 읽지 못해 복원하지 않았습니다. 파일은 {}에 남겨 두었습니다",
            path.display()
        ),
        None => "저장하지 않은 변경 내용의 백업 파일을 읽지 못해 복원하지 않았습니다".to_owned(),
    }
}

/// The notice for a backed-up buffer whose file could not be opened.
pub fn restore_failed_notice(name: &str, reason: &str) -> String {
    format!("{name}의 저장하지 않은 변경 내용을 복원하지 못했습니다: {reason}. 백업은 남아 있어 다음에 창을 열 때 다시 시도합니다")
}

struct Live {
    tracker: Tracker,
    dir: PathBuf,
    /// Files of finished windows whose buffers this window took over; removed once its own file holds them.
    sources: Vec<PathBuf>,
    pid: u32,
}

/// The tracker of this process's editor window. Global, not window state: the exit path of
/// `FR-L31.1` runs after the window's own state may be gone, and the debounced write runs on the
/// app runtime.
static LIVE: Mutex<Option<Live>> = Mutex::new(None);

fn live() -> std::sync::MutexGuard<'static, Option<Live>> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn flush_live(only_seq: Option<u64>) {
    let mut guard = live();
    let Some(live) = guard.as_mut() else { return };
    if only_seq.is_some_and(|seq| seq != live.tracker.seq()) {
        // A newer change started its own wait.
        return;
    }
    match live.tracker.flush(&live.dir, unix_ms()) {
        Ok(Flush::Unchanged) => {}
        Ok(done) => {
            tracing::debug!(?done, "editor backup");
            // The buffers of the finished windows are in this window's own file now (or were all dealt with).
            for source in std::mem::take(&mut live.sources) {
                if let Err(error) = std::fs::remove_file(&source) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!("could not remove the editor backup {}: {error}", source.display());
                    }
                }
            }
        }
        Err(error) => tracing::warn!("could not write the editor backup in {}: {error}", live.dir.display()),
    }
}

/// Writes the backup now if it is behind. Called on the exit path of `FR-L31.1` (`shutdown`): the
/// 에디터 닫기 menu item and the return of the window loop. Nothing in a process without an editor
/// window.
pub fn flush_on_exit() {
    flush_live(None);
    // This window is finished: its file is no longer a running window's.
    if let Some(live) = live().as_ref() {
        let _ = std::fs::remove_file(live.dir.join(live_marker_name(live.tracker.key(), live.pid)));
    }
}

pub mod view {
    //! The window's side: three calls from the workbench page.

    use std::cell::Cell;

    use dioxus_compose::html::prelude::dioxus_signals::{ReadableExt, WritableExt};

    use super::{backup_dir, collect_for_key, live, live_marker_name, marker_is_live, prune_dir, restore_failed_notice, touch_marker, unix_ms, unreadable_notice, window_file_name, BackupKey, DocView, Live, Tracker, WriteSchedule, HEARTBEAT_MS};
    use crate::editor_host::HostCommand;
    use crate::ui::editor::{base_name, display_name, file_failure_reason};
    use crate::ui::editor_states::Reopen;
    use crate::ui::editor_workbench::Shared;

    /// When the next write is due; the waiting task runs on the app runtime.
    static SCHEDULE: std::sync::Mutex<WriteSchedule> = std::sync::Mutex::new(WriteSchedule { first: None, last: 0, running: false });

    thread_local! {
        /// The backup of this window was looked for (once per process: one editor window each).
        static STARTED: Cell<bool> = const { Cell::new(false) };
        /// The host's reconnect count the tracker was already told about.
        static RECONNECTS_SEEN: Cell<u64> = const { Cell::new(0) };
    }

    fn notice(text: impl Into<String>) {
        crate::ui::cr::notice::post(text);
    }

    /// Reads the window's backup and drops the expired ones, the first time it is called. Without a
    /// data folder there is no hot exit.
    fn start(s: &Shared) {
        if STARTED.with(|started| started.replace(true)) {
            return;
        }
        let services = crate::services::services();
        let Some(dir) = services.config.data_dir.as_deref().map(backup_dir) else { return };
        let days = services.ui.prefs.get_untracked().hot_exit_backup_days;
        let now = unix_ms();
        for path in prune_dir(&dir, now, days) {
            tracing::info!("removed an editor backup older than {days} days: {}", path.display());
        }
        super::prune_markers(&dir, std::time::SystemTime::now());
        let key = BackupKey::new(&s.computer, &s.project, &s.folder);
        let pid = std::process::id();
        // Say this window is running before looking at the others, so one opening at the same moment leaves its file alone.
        if let Err(error) = touch_marker(&dir, &key, pid) {
            tracing::warn!("could not mark the editor window as running in {}: {error}", dir.display());
        }
        let rt = services.rt.handle().clone();
        let heartbeat = (dir.clone(), key.clone());
        rt.spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(HEARTBEAT_MS)).await;
                let _ = touch_marker(&heartbeat.0, &heartbeat.1, pid);
            }
        });
        let now_system = std::time::SystemTime::now();
        let collected = collect_for_key(&dir, &key, pid, now, days, |other| marker_is_live(&dir.join(live_marker_name(&key, other)), now_system));
        for (reason, kept_as) in &collected.unreadable {
            tracing::warn!("ignoring an editor backup of {}: {reason}", key.folder);
            notice(unreadable_notice(kept_as.as_deref()));
        }
        let mut tracker = Tracker::new(key.clone(), collected.merged).with_file_name(window_file_name(&key, pid));
        if !collected.sources.is_empty() {
            // Own the taken-over buffers in this window's file before the files they came from go.
            tracker.mark_changed();
        }
        *live() = Some(Live { tracker, dir, sources: collected.sources, pid });
        super::flush_live(None);
    }

    /// One session update, before the window applies it: an open tells whether a restored buffer's
    /// file changed on disk; a failed open keeps the buffer in the backup and says so.
    pub fn on_update(s: &Shared, update: &ember_editor::SessionUpdate) {
        match update {
            ember_editor::SessionUpdate::Opened { uri, text, .. } => {
                start(s);
                if let Some(live) = live().as_mut() {
                    live.tracker.opened(&uri.path, text);
                }
            }
            ember_editor::SessionUpdate::OpenFailed { uri, error } => {
                start(s);
                let first = live().as_mut().is_some_and(|live| live.tracker.open_failed(&uri.path));
                if first {
                    notice(restore_failed_notice(&display_name(base_name(&uri.path)), file_failure_reason(error)));
                }
            }
            _ => {}
        }
    }

    /// After the window applied the session's updates: put the backup's buffers back once the
    /// session is connected, follow the documents, raise the conflict bar of a restored buffer whose
    /// file changed, and start the debounced write when something changed.
    pub fn after_updates(s: &Shared) {
        start(s);
        let (connected, reconnects) = {
            let state = s.host.state.lock().unwrap();
            (state.connected, state.reconnects)
        };
        let reconnected = RECONNECTS_SEEN.with(|c| c.replace(reconnects)) != reconnects;
        let plan = match live().as_mut() {
            Some(live) => {
                if reconnected {
                    live.tracker.reconnected();
                }
                if connected { live.tracker.take_restore_plan() } else { Vec::new() }
            }
            None => return,
        };
        for step in plan {
            match step {
                Reopen::Restore { path, text } => {
                    let mut tabs = s.tabs;
                    tabs.write().open(&path);
                    s.open_errors.borrow_mut().remove(&path);
                    s.host.send(HostCommand::Restore { path, text });
                }
                Reopen::Open(path) => {
                    s.host.send(HostCommand::Open(path));
                }
                Reopen::Focus(path) => {
                    let mut tabs = s.tabs;
                    tabs.write().activate(&path);
                    s.host.send(HostCommand::Focus(path));
                }
            }
        }
        let observed = {
            let tab_paths: Vec<String> = s.tabs.read().paths().into_iter().map(str::to_owned).collect();
            let current = s.tabs.read().current_path().map(str::to_owned);
            let bridge = s.bridge.borrow();
            let uris = s.uris.borrow();
            // Tab order first, then documents without a tab (the working tree of an open diff, FR-L31.10).
            let mut others: Vec<&String> = uris.keys().filter(|p| !tab_paths.contains(*p)).collect();
            others.sort();
            let docs: Vec<DocView<'_>> = tab_paths
                .iter()
                .chain(others)
                .filter_map(|path| {
                    let doc = bridge.document(uris.get(path)?)?;
                    Some(DocView { path: path.as_str(), generation: doc.generation, version: doc.version, dirty: doc.dirty, text: doc.text.as_str() })
                })
                .collect();
            let mut guard = live();
            let Some(live) = guard.as_mut() else { return };
            live.tracker.observe(&docs, current.as_deref())
        };
        for path in &observed.conflicts {
            let Some(uri) = s.uris.borrow().get(path).cloned() else { continue };
            // A file that is gone is marked deleted by the session; that mark stays.
            let free = s.bridge.borrow().document(&uri).is_some_and(|doc| doc.external_change.is_none());
            if free {
                let _ = s.bridge.borrow_mut().apply(ember_editor::SessionUpdate::ExternalChange { uri, change: ember_editor::ExternalChange::ChangedWhileDirty });
            }
        }
        if observed.changed && SCHEDULE.lock().unwrap_or_else(|e| e.into_inner()).note_change(unix_ms()) {
            crate::services::services().rt.spawn(async move {
                loop {
                    let wait = SCHEDULE.lock().unwrap_or_else(|e| e.into_inner()).due_in(unix_ms());
                    match wait {
                        None => break,
                        Some(0) => {
                            SCHEDULE.lock().unwrap_or_else(|e| e.into_inner()).written();
                            super::flush_live(None);
                        }
                        Some(ms) => tokio::time::sleep(std::time::Duration::from_millis(ms)).await,
                    }
                }
            });
        }
    }
}
