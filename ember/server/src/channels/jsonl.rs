//! The per-channel JSONL file of human messages (SPEC FR-T16) [provisional layout].
//!
//! `<data dir>/channels/<channel id>.jsonl`, one JSON object per line:
//! `{"seq":N,"channel_id":"...","from":"user","text":"...","at":S}` (`at` is Unix seconds, as in
//! the API). A guest's line (a guest is a human, FR-T15) has `from` = `guest:<id>` and one more
//! field, `name`, the display name at the time of writing; no other line has `name`. The file is append-only: each line is one `write_all` on a handle opened in append
//! mode, and nothing is ever rewritten or truncated. There is no fsync (the SQLite log is the
//! read index; this file is the human-only record). Only messages from [`USER`] and guests are
//! written; agent and system messages stay in SQLite alone.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde_json::json;

use super::{ChannelError, Message, USER};

/// Longest channel id accepted as a file name.
const MAX_ID: usize = 128;

#[derive(Debug)]
pub struct JsonlLog {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl JsonlLog {
    /// A log writing under `<data_dir>/channels/`. The directory is created on first append.
    pub fn new(data_dir: &Path) -> Self {
        JsonlLog { dir: data_dir.join("channels"), lock: Mutex::new(()) }
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

    /// Append one line for a human message (`user` or a guest); anything else is skipped
    /// (`Ok(false)`), so agent and system messages never reach the file.
    pub fn append(&self, channel_id: &str, message: &Message) -> Result<bool, ChannelError> {
        let guest = message.from.starts_with(super::guest::AUTHOR_PREFIX);
        if message.from != USER && !guest {
            return Ok(false);
        }
        let path = self.path_of(channel_id)?;
        let mut value = json!({
            "seq": message.seq,
            "channel_id": channel_id,
            "from": message.from,
            "text": message.text,
            "at": message.at,
        });
        if guest {
            value["name"] = json!(message.author_name);
        }
        let mut line = value.to_string();
        line.push('\n');
        let io = |e: std::io::Error| ChannelError::Other(anyhow::anyhow!("channel log {}: {e}", path.display()));
        std::fs::create_dir_all(&self.dir).map_err(io)?;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(io)?;
        f.write_all(line.as_bytes()).map_err(io)?;
        Ok(true)
    }
}
