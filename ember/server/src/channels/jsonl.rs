//! The per-channel JSONL file of human messages (SPEC FR-T16) [provisional layout].
//!
//! `<data dir>/channels/<channel id>.jsonl`, one JSON object per line:
//! `{"seq":N,"channel_id":"...","from":"user","text":"...","at":S}` (`at` is Unix seconds, as in
//! the API). A guest's line (a guest is a human, FR-T15) has `from` = `guest:<id>` and one more
//! field, `name`, the display name at the time of writing; no other line has `name`. Only
//! messages from [`USER`] and guests are written; agent and system messages stay in SQLite alone.
//!
//! Writing. The file is append-only: each line is one `write_all` on a handle opened in append
//! mode, followed by `sync_data`, and a failed write or sync is an append failure (the caller
//! rolls the row back). The directory is created with mode 0700 when it does not exist (an
//! existing one is never changed), a new file gets mode 0600, the file is opened with
//! `O_NOFOLLOW | O_NONBLOCK` (a symlinked `<id>.jsonl` is refused and a FIFO cannot block the open), and the opened handle must be a regular
//! file owned by the effective user. Error messages carry no path and no message text.
//!
//! Size cap. A file larger than [`MAX_FILE_BYTES`] takes no further lines. The post is NOT
//! refused (that would make the channel unwritable): `append` returns `Ok(false)`, the SQLite row
//! stays, and a warning is logged once per channel and process.
//!
//! Recovery. [`JsonlLog::recover`] runs once at start. SQLite is the index and this file the
//! human-only record, so a crash or a failed rollback can leave the file behind the store. For
//! each channel it reads only the last [`RECOVER_WINDOW`] bytes: a final line without `\n` is a
//! crash fragment and the file is cut back to the last `\n`; then the human rows of SQLite with
//! a `seq` above the last line's `seq` are appended in order. Limit: the repair is append-only,
//! so a row missing from the middle of the file stays missing. A file that is a symlink, not a
//! regular file, owned by another user, over the cap, or whose last line cannot be parsed is
//! left untouched and logged. Recovery never fails the start.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde_json::json;

use super::{ChannelError, Message, USER};
use crate::store::Store;

/// A channel file larger than this takes no further lines (see the module doc).
pub const MAX_FILE_BYTES: u64 = 1 << 30;

/// How much of the end of a file recovery reads (1 MiB).
const RECOVER_WINDOW: u64 = 1 << 20;

/// What [`JsonlLog::recover`] did, in counts only.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecoverReport {
    /// Channels that have a file or human rows in the store.
    pub channels_checked: usize,
    /// Bytes cut from the end of files (crash fragments).
    pub truncated_bytes: u64,
    /// Lines appended from the store.
    pub repaired_lines: usize,
    /// Channels left untouched because of a warning.
    pub skipped: usize,
}

fn is_human(message: &Message) -> bool {
    message.from == USER || message.from.starts_with(super::guest::AUTHOR_PREFIX)
}

/// The line (with its newline) for a human message; `None` for any other.
fn render_line(channel_id: &str, message: &Message) -> Option<String> {
    if !is_human(message) {
        return None;
    }
    let mut value = json!({
        "seq": message.seq,
        "channel_id": channel_id,
        "from": message.from,
        "text": message.text,
        "at": message.at,
    });
    if message.from != USER {
        value["name"] = json!(message.author_name);
    }
    let mut line = value.to_string();
    line.push('\n');
    Some(line)
}

/// Open a channel file for reading and appending, never through a symlink, mode 0600 when new.
fn open_log(path: &Path, create: bool) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true).append(true);
    if create {
        o.create(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    o.open(path)
}

/// The opened handle is a regular file owned by the effective user.
fn handle_ok(f: &File) -> bool {
    let Ok(md) = f.metadata() else { return false };
    if !md.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and cannot fail.
        if md.uid() != unsafe { libc::geteuid() } {
            return false;
        }
    }
    true
}

/// From the tail window of a file of `len` bytes: where the file should end (just after the last
/// `\n`) and the `seq` of the last complete line (0 when there is none).
fn scan_tail(f: &mut File, len: u64) -> Result<(u64, i64), &'static str> {
    if len == 0 {
        return Ok((0, 0));
    }
    let window = len.min(RECOVER_WINDOW);
    let start = len - window;
    f.seek(SeekFrom::Start(start)).map_err(|_| "cannot read the log file")?;
    let mut buf = vec![0u8; window as usize];
    f.read_exact(&mut buf).map_err(|_| "cannot read the log file")?;
    let keep_in_buf = match buf.iter().rposition(|&b| b == b'\n') {
        Some(i) => i + 1,
        None if start > 0 => return Err("last line too long"),
        None => 0,
    };
    let keep = start + keep_in_buf as u64;
    if keep_in_buf == 0 {
        // The whole file is one fragment.
        return Ok((0, 0));
    }
    let body = &buf[..keep_in_buf - 1];
    let line_start = match body.iter().rposition(|&b| b == b'\n') {
        Some(i) => i + 1,
        None if start > 0 => return Err("last line too long"),
        None => 0,
    };
    let v: serde_json::Value = serde_json::from_slice(&body[line_start..]).map_err(|_| "last line is not valid JSON")?;
    let seq = v.get("seq").and_then(|s| s.as_i64()).ok_or("last line has no integer seq")?;
    Ok((keep, seq))
}

/// Longest channel id accepted as a file name.
const MAX_ID: usize = 128;

#[derive(Debug)]
pub struct JsonlLog {
    dir: PathBuf,
    lock: Mutex<()>,
    max_file_bytes: u64,
    /// Channels whose cap warning was logged.
    capped: Mutex<HashSet<String>>,
}

impl JsonlLog {
    /// A log writing under `<data_dir>/channels/`. The directory is created on first append.
    pub fn new(data_dir: &Path) -> Self {
        Self::with_max_file_bytes(data_dir, MAX_FILE_BYTES)
    }

    /// As [`JsonlLog::new`] with another size cap (tests use a small one).
    pub fn with_max_file_bytes(data_dir: &Path, max_file_bytes: u64) -> Self {
        JsonlLog { dir: data_dir.join("channels"), lock: Mutex::new(()), max_file_bytes, capped: Mutex::new(HashSet::new()) }
    }

    /// Create the channels directory with mode 0700 if missing; an existing one is left as is.
    fn ensure_dir(&self) -> std::io::Result<()> {
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(&self.dir)
    }

    /// Held by a caller across "insert row, append line, roll back on failure", so lines reach
    /// the file in `seq` order.
    pub fn guard(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The file of a channel, or an error when the id is not a safe path component.
    pub fn path_of(&self, channel_id: &str) -> Result<PathBuf, ChannelError> {
        let safe = !channel_id.is_empty()
            && channel_id.len() <= MAX_ID
            && channel_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !safe {
            return Err(ChannelError::BadRequest(format!("unsafe channel id {channel_id:?}")));
        }
        Ok(self.dir.join(format!("{channel_id}.jsonl")))
    }

    /// Append one line for a human message (`user` or a guest) and sync it. `Ok(false)` when
    /// nothing was written: the message is not human (agent and system messages never reach the
    /// file) or the file is over the size cap (the row stays in SQLite alone).
    pub fn append(&self, channel_id: &str, message: &Message) -> Result<bool, ChannelError> {
        if !is_human(message) {
            return Ok(false);
        }
        let path = self.path_of(channel_id)?;
        let Some(line) = render_line(channel_id, message) else { return Ok(false) };
        let fail = |what: &str, e: std::io::Error| ChannelError::Other(anyhow::anyhow!("channel log {what}: {}", e.kind()));
        self.ensure_dir().map_err(|e| fail("directory", e))?;
        let mut f = open_log(&path, true).map_err(|e| fail("open", e))?;
        if !handle_ok(&f) {
            return Err(ChannelError::Other(anyhow::anyhow!("channel log refused: not a regular file of this user")));
        }
        let len = f.metadata().map_err(|e| fail("stat", e))?.len();
        if len > self.max_file_bytes {
            let first = self.capped.lock().unwrap_or_else(|e| e.into_inner()).insert(channel_id.to_string());
            if first {
                tracing::warn!(channel = %channel_id, "channel log over the size cap, new lines are not written");
            }
            return Ok(false);
        }
        f.write_all(line.as_bytes()).map_err(|e| fail("write", e))?;
        f.sync_data().map_err(|e| fail("sync", e))?;
        Ok(true)
    }

    /// Repair the files against the store at start (see the module doc). Never fails: every
    /// problem is a warning naming the channel id and a fixed reason.
    ///
    /// ORDER MATTERS: this must run on a `JsonlLog` of its own BEFORE any router that appends is
    /// served. The routers each build their own `JsonlLog` (their own `guard()` mutex), so this
    /// function shares no lock with them; it is safe only because `main` calls it synchronously,
    /// before the owner and guest routers exist (a review finding of #669). A crash fragment is
    /// cut in place with `set_len`: its bytes are not saved aside (every human message is also in
    /// SQLite and is re-appended below).
    pub fn recover(&self, store: &Store) -> RecoverReport {
        let mut report = RecoverReport::default();
        let channels = match store.channels(None) {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!("channel log recovery: cannot list the channels");
                return report;
            }
        };
        for channel in channels {
            let _guard = self.guard();
            if let Err(reason) = self.recover_channel(store, &channel.id, &mut report) {
                report.skipped += 1;
                tracing::warn!(channel = %channel.id, reason, "channel log recovery skipped a channel");
            }
        }
        if report != RecoverReport::default() {
            tracing::info!(
                channels_checked = report.channels_checked,
                truncated_bytes = report.truncated_bytes,
                repaired_lines = report.repaired_lines,
                skipped = report.skipped,
                "channel log recovery"
            );
        }
        report
    }

    fn recover_channel(&self, store: &Store, id: &str, report: &mut RecoverReport) -> Result<(), &'static str> {
        let path = self.path_of(id).map_err(|_| "unsafe channel id")?;
        let exists = match std::fs::symlink_metadata(&path) {
            Ok(md) if md.file_type().is_file() => true,
            Ok(_) => return Err("not a regular file"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => return Err("cannot stat the log file"),
        };
        // Everything is decided before the file is changed.
        let mut open = None;
        let (mut last_seq, mut keep, mut cut) = (0i64, 0u64, 0u64);
        if exists {
            let mut f = open_log(&path, false).map_err(|_| "cannot open the log file")?;
            if !handle_ok(&f) {
                return Err("not a regular file of this user");
            }
            let len = f.metadata().map_err(|_| "cannot stat the log file")?.len();
            if len > self.max_file_bytes {
                return Err("log file over the size cap");
            }
            (keep, last_seq) = scan_tail(&mut f, len)?;
            cut = len - keep;
            open = Some(f);
        }
        let pending: Vec<Message> = store
            .channel_messages_after(id, last_seq, None, false)
            .map_err(|_| "cannot read the channel from the store")?
            .into_iter()
            .filter(is_human)
            .collect();
        if !exists && pending.is_empty() {
            return Ok(());
        }
        report.channels_checked += 1;
        let mut f = match open {
            Some(f) => f,
            None => {
                self.ensure_dir().map_err(|_| "cannot create the channels directory")?;
                let f = open_log(&path, true).map_err(|_| "cannot open the log file")?;
                if !handle_ok(&f) {
                    return Err("not a regular file of this user");
                }
                f
            }
        };
        if cut > 0 {
            f.set_len(keep).map_err(|_| "cannot truncate the log file")?;
            report.truncated_bytes += cut;
        }
        if !pending.is_empty() {
            let buf: String = pending.iter().filter_map(|m| render_line(id, m)).collect();
            f.write_all(buf.as_bytes()).map_err(|_| "cannot write the log file")?;
            report.repaired_lines += pending.len();
        }
        if cut > 0 || !pending.is_empty() {
            f.sync_data().map_err(|_| "cannot sync the log file")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_t16_recovery_writes_a_guest_line_with_name() {
        let store = Store::open_in_memory().unwrap();
        store.create_session("core", crate::agents::AgentKind::Scripted, "/tmp/x", None, "alpha").unwrap();
        let channel = store.channels(None).unwrap().remove(0).id;
        store
            .conn()
            .execute(
                "INSERT INTO channel_messages (channel_id, author, body, at, mentions) VALUES (?1, 'guest:g1', 'hi', 5000, '[]')",
                rusqlite::params![channel],
            )
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let report = JsonlLog::new(dir.path()).recover(&store);
        assert_eq!(report.repaired_lines, 1);
        let text = std::fs::read_to_string(dir.path().join("channels").join(format!("{channel}.jsonl"))).unwrap();
        let v: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(v["from"], "guest:g1");
        assert_eq!(v["name"], "guest");
        assert_eq!(v["text"], "hi");
        assert_eq!(v["at"], 5);
    }
}
