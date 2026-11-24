//! Persistent storage for `MainThreadStorage` (`globalState` and `workspaceState`).
//!
//! Extensions read their state during activation via `$initializeExtensionStorage(shared, extensionId)`
//! and persist updates via `$setValue(shared, extensionId, value)`.
//!
//! * `shared = true` (`globalState`): one JSON file per extension under the global storage directory.
//! * `shared = false` (`workspaceState`): one JSON file per extension under a per-workspace
//!   subdirectory of the workspace storage directory, keyed by authority and workspace folder paths.
//!
//! Keys are encoded into single safe path components with a deterministic 64-bit hash suffix so
//! arbitrary extension ids or workspace paths can never escape their storage directory or collide
//! on case-insensitive filesystems.
//!
//! Writes are serialized per [`StorageKey`]: at most one write is in flight per key, overlapping
//! `$setValue` calls coalesce into a single latest pending value, `$setValue` answers immediately
//! without rejecting on filesystem errors, and failed writes retry with bounded backoff.
//! Transient read errors are never cached as absent and block writes until a read succeeds.
//! Corrupt JSON files are moved aside to `<path>.corrupt` before recovery or writes proceed.
//!
//! Note: extension-id storage migration (`extensionStorage.ts` migration maps) is not implemented.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ember_editor_conn::rpc::{js_error, Reply};
use ember_editor_conn::uri::UriComponents;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::engine::Token;

/// Subdirectory under the server's `globalStorageHome` / `workspaceStorageHome` where Ember stores
/// per-extension JSON files, kept separate from Code-OSS's own `state.vscdb` and extension folders.
pub const STORAGE_SUBDIR: &str = "ember-storage";

/// Maximum number of persistence attempts (initial attempt + retries) for a single write before
/// giving up and logging an error.
pub const MAX_WRITE_RETRIES: u32 = 5;

/// Number of retries after an initial transient read failure before rejecting pending
/// `$initializeExtensionStorage` waiters (1 initial attempt + 3 retries = 4 total attempts).
pub const MAX_READ_RETRIES: u32 = 3;

/// Base backoff delay for retrying a failed storage read or write.
const RETRY_BASE_DELAY: Duration = Duration::from_millis(50);

/// Monotonic sequence suffix for quarantine filenames so two corruptions never collide even
/// within the same millisecond.
static CORRUPT_BACKUP_SEQ: AtomicU64 = AtomicU64::new(1);

fn retry_delay(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(4);
    RETRY_BASE_DELAY.saturating_mul(1u32 << shift)
}

/// Broadcast payload emitted when one session updates a `shared = true` (`globalState`) key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedStorageChange {
    pub origin_session: u64,
    pub extension_id: String,
    pub value: Option<String>,
}

/// Cross-session broadcast bus for `shared = true` (`globalState`) updates on one storage scope.
#[derive(Debug, Clone)]
pub struct StorageHub {
    tx: broadcast::Sender<SharedStorageChange>,
    next_session: Arc<AtomicU64>,
}

impl Default for StorageHub {
    fn default() -> Self {
        Self::new()
    }
}

impl StorageHub {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self { tx, next_session: Arc::new(AtomicU64::new(1)) }
    }

    pub fn next_session_id(&self) -> u64 {
        self.next_session.fetch_add(1, Ordering::Relaxed)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SharedStorageChange> {
        self.tx.subscribe()
    }

    pub fn publish(&self, origin_session: u64, extension_id: String, value: Option<String>) {
        let _ = self.tx.send(SharedStorageChange { origin_session, extension_id, value });
    }
}

/// Deterministic 64-bit FNV-1a hash (stable across Rust versions and platforms).
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Sanitize a human-readable ASCII prefix for a single path component: only `[a-z0-9._-]`,
/// collapsing any `..` or repeated separators, stripping leading/trailing dots, capped at `max_len`.
fn safe_prefix(raw: &str, max_len: usize, allow_dot: bool, fallback: &str) -> String {
    let mut out = String::new();
    let mut prev_sep = false;
    for c in raw.chars() {
        let lower = c.to_ascii_lowercase();
        let ok = lower.is_ascii_lowercase()
            || lower.is_ascii_digit()
            || lower == '-'
            || lower == '_'
            || (allow_dot && lower == '.');
        if ok {
            if lower == '.' && out.ends_with('.') {
                continue;
            }
            out.push(lower);
            prev_sep = false;
        } else if !out.is_empty() && !prev_sep {
            out.push('_');
            prev_sep = true;
        }
        if out.len() >= max_len {
            break;
        }
    }
    let trimmed = out.trim_matches(|c| c == '.' || c == '_');
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Encode an extension id into a single safe filename stem (without `.json`).
///
/// Extension ids are case-insensitive in VS Code (`ExtensionIdentifier.toKey`), so the input is
/// lowercased before hashing. The result never contains path separators, `..`, NUL, or leading
/// dots, and distinct ids never collide even if their sanitized prefixes match.
pub fn encode_extension_key(extension_id: &str) -> String {
    let lower = extension_id.to_ascii_lowercase();
    let prefix = safe_prefix(&lower, 48, true, "ext");
    format!("{prefix}-{:016x}", fnv1a64(lower.as_bytes()))
}

/// Normalize a server authority so spelling differences (`localhost`, `127.0.0.1`, `[::1]`, case)
/// map to the same canonical key.
pub fn canonicalize_authority(authority: &str) -> String {
    let trimmed = authority.trim().to_ascii_lowercase();
    let (host, port, bracketed) = if let Some(rest) = trimmed.strip_prefix('[') {
        if let Some((h, after)) = rest.split_once(']') {
            (h, after.strip_prefix(':').unwrap_or(""), true)
        } else {
            (trimmed.as_str(), "", false)
        }
    } else if trimmed.matches(':').count() == 1 {
        let (h, p) = trimmed.split_once(':').unwrap();
        (h, p, false)
    } else {
        (trimmed.as_str(), "", false)
    };
    let norm_host = match host {
        "localhost" | "localhost." | "127.0.0.1" | "::1" | "0:0:0:0:0:0:0:1" => "localhost",
        other => other,
    };
    if norm_host == "localhost" {
        if port.is_empty() {
            "localhost".to_owned()
        } else {
            format!("localhost:{port}")
        }
    } else if bracketed {
        if port.is_empty() {
            format!("[{norm_host}]")
        } else {
            format!("[{norm_host}]:{port}")
        }
    } else if port.is_empty() {
        norm_host.to_owned()
    } else {
        format!("{norm_host}:{port}")
    }
}

fn normalize_workspace_paths<'a>(workspace_paths: &'a [&'a str]) -> Vec<&'a str> {
    workspace_paths
        .iter()
        .map(|p| {
            if *p == "/" {
                "/"
            } else {
                p.trim_end_matches(['/', '\\'])
            }
        })
        .collect()
}

fn workspace_leaf_prefix(normalized: &[&str]) -> String {
    let leaf = normalized
        .first()
        .and_then(|p| p.rsplit(['/', '\\']).find(|s| !s.is_empty()))
        .unwrap_or("ws");
    safe_prefix(leaf, 32, false, "ws")
}

/// Encode one or more workspace folder paths into a single safe directory name without an
/// authority component. Used in remote mode where storage already lives under that server's own
/// `workspaceStorageHome`, so endpoint spelling or port changes never orphan `workspaceState`.
pub fn encode_workspace_paths(workspace_paths: &[&str]) -> String {
    let normalized = normalize_workspace_paths(workspace_paths);
    let prefix = workspace_leaf_prefix(&normalized);
    let mut input = Vec::new();
    for (idx, p) in normalized.iter().enumerate() {
        if idx > 0 {
            input.push(0);
        }
        input.extend_from_slice(p.as_bytes());
    }
    format!("{prefix}-{:016x}", fnv1a64(&input))
}

/// Encode a workspace identity (canonicalized server authority plus one or more workspace folder
/// paths) into a single safe directory name.
///
/// Trailing slashes are stripped from each path (except `"/"` itself) so `/work/app` and
/// `/work/app/` share the same workspace storage. Including the canonicalized authority guarantees
/// that two servers with the same workspace path never collide in local `workspaceState`, while
/// loopback spellings (`localhost`, `127.0.0.1`, `[::1]`) stay unified.
pub fn encode_workspace_identity(authority: &str, workspace_paths: &[&str]) -> String {
    let norm_authority = canonicalize_authority(authority);
    let normalized = normalize_workspace_paths(workspace_paths);
    let prefix = workspace_leaf_prefix(&normalized);

    let mut input = Vec::new();
    input.extend_from_slice(norm_authority.as_bytes());
    for p in &normalized {
        input.push(0);
        input.extend_from_slice(p.as_bytes());
    }
    format!("{prefix}-{:016x}", fnv1a64(&input))
}

/// Encode a single-folder workspace identity for `authority` and `workspace_path`.
pub fn encode_workspace_key(authority: &str, workspace_path: &str) -> String {
    encode_workspace_identity(authority, &[workspace_path])
}

fn join_uri_path(base: &str, child: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.is_empty() {
        format!("/{child}")
    } else {
        format!("{trimmed}/{child}")
    }
}

fn uri_with_path(base: &UriComponents, path: &str) -> UriComponents {
    UriComponents::new(&base.scheme, &base.authority, path)
}

/// Unique sibling quarantine URI for a corrupt storage file (`<path>.corrupt.<millis>-<seq>`), so
/// quarantining a second corrupt file never overwrites an earlier backup.
pub fn corrupt_backup_uri(uri: &UriComponents) -> UriComponents {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = CORRUPT_BACKUP_SEQ.fetch_add(1, Ordering::Relaxed);
    uri_with_path(uri, &format!("{}.corrupt.{millis}-{seq}", uri.path))
}

/// The global and workspace storage directory URIs for one editor session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRoots {
    /// Directory for global extension storage (`shared: true`).
    pub global_dir: UriComponents,
    /// Directory for this workspace's extension storage (`shared: false`).
    pub workspace_dir: UriComponents,
}

impl StorageRoots {
    /// Derive storage roots under the OSE server's `globalStorageHome` and `workspaceStorageHome`
    /// using `vscode-remote://<authority>` URIs so all I/O stays on the server via `RemoteFs`.
    pub fn from_env(
        authority: &str,
        global_storage_home: &UriComponents,
        workspace_storage_home: &UriComponents,
        workspace_path: &str,
    ) -> Self {
        Self::from_env_folders(authority, global_storage_home, workspace_storage_home, &[workspace_path])
    }

    /// Multi-root variant of [`Self::from_env`].
    ///
    /// In remote mode, storage already lives under the server's `workspaceStorageHome`, so the
    /// workspace directory key hashes only the normalized workspace folder paths without tying to
    /// endpoint spelling or port.
    pub fn from_env_folders(
        authority: &str,
        global_storage_home: &UriComponents,
        workspace_storage_home: &UriComponents,
        workspace_paths: &[&str],
    ) -> Self {
        let global_path = join_uri_path(&global_storage_home.path, STORAGE_SUBDIR);
        let ws_key = encode_workspace_paths(workspace_paths);
        let ws_base = join_uri_path(&workspace_storage_home.path, STORAGE_SUBDIR);
        let ws_path = join_uri_path(&ws_base, &ws_key);
        Self {
            global_dir: UriComponents::remote(authority, &global_path),
            workspace_dir: UriComponents::remote(authority, &ws_path),
        }
    }

    /// Derive storage roots under a local directory on the client machine.
    ///
    /// Includes `authority` in the workspace identity hash so two servers with the same
    /// `workspace_path` never share `workspaceState`.
    pub fn from_local_dir(root: &Path, authority: &str, workspace_path: &str) -> Self {
        Self::from_local_dir_folders(root, authority, &[workspace_path])
    }

    /// Multi-root variant of [`Self::from_local_dir`].
    pub fn from_local_dir_folders(root: &Path, authority: &str, workspace_paths: &[&str]) -> Self {
        let global = root.join("global");
        let workspace = root.join("workspace").join(encode_workspace_identity(authority, workspace_paths));
        Self {
            global_dir: UriComponents::file(global.to_string_lossy().as_ref()),
            workspace_dir: UriComponents::file(workspace.to_string_lossy().as_ref()),
        }
    }

    /// The file URI for `(shared, extension_id)`.
    pub fn file_uri(&self, shared: bool, extension_id: &str) -> UriComponents {
        let dir = if shared { &self.global_dir } else { &self.workspace_dir };
        let file_name = format!("{}.json", encode_extension_key(extension_id));
        let path = join_uri_path(&dir.path, &file_name);
        uri_with_path(dir, &path)
    }
}

/// Cache key for one extension's global or workspace storage.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StorageKey {
    pub shared: bool,
    pub extension_id: String,
}

impl StorageKey {
    pub fn new(shared: bool, extension_id: &str) -> Self {
        Self { shared, extension_id: extension_id.to_ascii_lowercase() }
    }
}

/// Validate persisted storage bytes from disk.
///
/// Returns `Ok(Some(json_string))` when the file holds valid non-null JSON, `Ok(None)` for `null`
/// or empty whitespace, and `Err(reason)` for invalid UTF-8 or corrupt JSON.
pub fn decode_persisted_bytes(bytes: &[u8]) -> Result<Option<String>, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("not valid UTF-8: {e}"))?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let parsed: Value = serde_json::from_str(trimmed).map_err(|e| format!("corrupt JSON: {e}"))?;
    if parsed.is_null() {
        return Ok(None);
    }
    Ok(Some(trimmed.to_owned()))
}

/// Read a local storage file, mapping `NotFound` to `Ok(None)`.
pub fn read_local_optional(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Move a corrupt local storage file aside to `to` (`*.corrupt`).
pub fn quarantine_local(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Atomically write or delete a local storage file.
///
/// Writes to a sibling temporary file in the same directory and renames it into place so readers
/// never observe a partially written file. Passing `None` deletes the file (ignoring `NotFound`).
pub fn write_local_atomic(path: &Path, bytes: Option<&[u8]>) -> std::io::Result<()> {
    let Some(bytes) = bytes else {
        return match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        };
    };
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "storage path has no parent directory")
    })?;
    std::fs::create_dir_all(parent)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("storage.json");
    let tmp = parent.join(format!(".{name}.tmp.{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&tmp, bytes)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// What the core must do for a `$initializeExtensionStorage` request.
#[derive(Debug, Clone, PartialEq)]
pub enum InitAction {
    /// Answer immediately (value is in memory, or no persistence roots are configured).
    Reply(Reply),
    /// Issue a storage read; `req` is queued until [`ExtensionStorage::on_read_done`].
    Read { uri: UriComponents, key: StorageKey },
    /// A read or quarantine for this key is already in flight; `req` has been queued on it.
    Wait,
}

/// One persistence read to execute for a single [`StorageKey`].
#[derive(Debug, Clone, PartialEq)]
pub struct ReadCommand {
    pub uri: UriComponents,
    pub key: StorageKey,
}

/// One persistence write to execute for a single [`StorageKey`].
#[derive(Debug, Clone, PartialEq)]
pub struct WriteCommand {
    pub uri: UriComponents,
    pub bytes: Option<Vec<u8>>,
    pub token: Token,
}

/// Result of [`ExtensionStorage::on_read_done`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReadOutcome {
    /// Replies for waiting `$initializeExtensionStorage` requests.
    pub replies: Vec<(u32, Result<Reply, Option<Value>>)>,
    /// When the file holds corrupt JSON, move it aside from `from` to `to` (`*.corrupt.<millis>-<seq>`)
    /// before resolving waiting initialize requests or allowing any writes.
    pub quarantine: Option<(UriComponents, UriComponents, StorageKey)>,
    /// A queued write that waited behind this read and can now proceed.
    pub write: Option<WriteCommand>,
}

/// Result of [`ExtensionStorage::on_tick`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TickOutcome {
    pub writes: Vec<WriteCommand>,
    pub reads: Vec<ReadCommand>,
}

/// Result of [`ExtensionStorage::on_quarantine_done`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QuarantineOutcome {
    /// Replies for waiting `$initializeExtensionStorage` requests.
    pub replies: Vec<(u32, Result<Reply, Option<Value>>)>,
    /// A write that queued while the corrupt file was being moved aside.
    pub write: Option<WriteCommand>,
}

/// Result of [`ExtensionStorage::on_set_value`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SetOutcome {
    /// Write to start immediately when no read/write/quarantine is in flight for this key.
    pub write: Option<WriteCommand>,
    /// When `shared == true`, `(extension_id, value)` to broadcast to other sessions.
    pub broadcast: Option<(String, Option<String>)>,
}

#[derive(Debug, Clone)]
struct PendingRead {
    waiters: Vec<u32>,
    attempts_failed: u32,
    /// `Some(due)` while waiting for backoff before issuing the next read attempt;
    /// `None` while a read attempt is currently in flight.
    retry_due: Option<Instant>,
}

#[derive(Debug, Clone)]
struct InFlightWrite {
    bytes: Option<Vec<u8>>,
    attempt: u32,
    /// True when an external shared change superseded this write while it was in flight; if the
    /// write fails, it must not schedule a retry over the newer external value.
    superseded_by_external: bool,
}

#[derive(Debug, Clone)]
struct ScheduledRetry {
    bytes: Option<Vec<u8>>,
    attempt: u32,
    due: Instant,
}

/// Pure state machine for `MainThreadStorage`: in-memory cache, coalesced reads with retry
/// backoff, corrupt-file quarantine, serialized per-key writes with retry backoff, external
/// shared-storage reload tracking, and registered sync keys.
#[derive(Debug, Default)]
pub struct ExtensionStorage {
    roots: Option<StorageRoots>,
    cache: HashMap<StorageKey, Option<String>>,
    pending_reads: HashMap<StorageKey, PendingRead>,
    /// Keys whose last read (after retries) or quarantine failed; writes are blocked until a read
    /// succeeds so an unreadable file is never silently overwritten.
    read_failed: HashSet<StorageKey>,
    /// Keys currently moving a corrupt file aside to `*.corrupt.<millis>-<seq>`, holding waiting
    /// init requests.
    quarantining: HashMap<StorageKey, Vec<u32>>,
    /// At most one in-flight write per key.
    in_flight_writes: HashMap<StorageKey, InFlightWrite>,
    /// Reverse lookup from write token to its key.
    write_tokens: HashMap<Token, StorageKey>,
    /// Latest pending value waiting behind an in-flight read, write, or quarantine for the same key.
    queued_writes: HashMap<StorageKey, Option<Vec<u8>>>,
    /// Failed writes scheduled for retry after a backoff delay.
    retry_writes: HashMap<StorageKey, ScheduledRetry>,
    /// Reverse lookup from `<encode_extension_key(id)>.json` to lowercased `extension_id` for
    /// matching directory watch events on `global_dir`.
    shared_file_index: HashMap<String, String>,
    /// Monotonic per-key generation bumped on every accepted `$setValue` and every applied
    /// external shared-storage change so a stale reload reply cannot rewind a newer value.
    generations: HashMap<StorageKey, u64>,
    sync_keys: HashMap<String, Vec<String>>,
}

impl ExtensionStorage {
    fn generation_of(&self, key: &StorageKey) -> u64 {
        self.generations.get(key).copied().unwrap_or(0)
    }

    fn bump_generation(&mut self, key: &StorageKey) -> u64 {
        let slot = self.generations.entry(key.clone()).or_insert(0);
        *slot += 1;
        *slot
    }
    pub fn set_roots(&mut self, roots: StorageRoots) {
        self.roots = Some(roots);
    }

    pub fn roots(&self) -> Option<&StorageRoots> {
        self.roots.as_ref()
    }

    /// Remember an extension id so file watcher events on `global_dir` can map its hashed filename
    /// back to the extension id.
    pub fn register_shared_extension(&mut self, extension_id: &str) {
        let lower = extension_id.to_ascii_lowercase();
        let file_name = format!("{}.json", encode_extension_key(&lower));
        self.shared_file_index.insert(file_name, lower);
    }

    /// Cached JSON string for `(shared, extension_id)`: `None` if not loaded yet,
    /// `Some(None)` if known absent, `Some(Some(raw))` if present.
    pub fn cached(&self, shared: bool, extension_id: &str) -> Option<Option<&str>> {
        self.cache.get(&StorageKey::new(shared, extension_id)).map(|v| v.as_deref())
    }

    /// Keys registered via `$registerExtensionStorageKeysToSync` for `extension_id`.
    pub fn sync_keys(&self, extension_id: &str) -> Option<&[String]> {
        self.sync_keys.get(&extension_id.to_ascii_lowercase()).map(Vec::as_slice)
    }

    /// Whether any storage write, queued write, retry, or corrupt-file quarantine is still pending.
    pub fn has_pending_writes(&self) -> bool {
        !self.in_flight_writes.is_empty()
            || !self.queued_writes.is_empty()
            || !self.retry_writes.is_empty()
            || !self.quarantining.is_empty()
    }

    /// Earliest deadline for a scheduled storage read or write retry, if any.
    pub fn next_wake(&self) -> Option<Instant> {
        let write_wake = self.retry_writes.values().map(|r| r.due);
        let read_wake = self.pending_reads.values().filter_map(|p| p.retry_due);
        write_wake.chain(read_wake).min()
    }

    /// Resolve a changed file path under `global_dir` to `(file_uri, extension_id, generation)`
    /// for reloading, skipping keys with a local write or quarantine currently in progress.
    pub fn shared_extension_for_path(&self, path: &str) -> Option<(UriComponents, String, u64)> {
        let roots = self.roots.as_ref()?;
        let dir = roots.global_dir.path.trim_end_matches('/');
        let rest = path.strip_prefix(dir)?.strip_prefix('/')?;
        if rest.contains('/') {
            return None;
        }
        let ext_id = self.shared_file_index.get(rest)?;
        let key = StorageKey::new(true, ext_id);
        if self.in_flight_writes.contains_key(&key)
            || self.queued_writes.contains_key(&key)
            || self.quarantining.contains_key(&key)
        {
            return None;
        }
        Some((roots.file_uri(true, ext_id), ext_id.clone(), self.generation_of(&key)))
    }

    /// All cached `shared = true` keys that should be reloaded from disk after a broadcast lag,
    /// skipping keys with a local write or quarantine currently in progress.
    pub fn cached_shared_keys(&self) -> Vec<(UriComponents, String, u64)> {
        let Some(roots) = self.roots.as_ref() else {
            return Vec::new();
        };
        self.cache
            .keys()
            .filter(|k| {
                k.shared
                    && !self.in_flight_writes.contains_key(k)
                    && !self.queued_writes.contains_key(k)
                    && !self.quarantining.contains_key(k)
            })
            .map(|k| (roots.file_uri(true, &k.extension_id), k.extension_id.clone(), self.generation_of(k)))
            .collect()
    }

    /// Handle `MainThreadStorage.$initializeExtensionStorage(shared, extensionId)`.
    pub fn on_initialize(&mut self, req: u32, shared: bool, extension_id: &str) -> InitAction {
        let key = StorageKey::new(shared, extension_id);
        if shared {
            self.register_shared_extension(&key.extension_id);
        }
        if let Some(entry) = self.cache.get(&key) {
            return InitAction::Reply(reply_for_entry(entry.as_deref()));
        }
        let Some(roots) = &self.roots else {
            return InitAction::Reply(Reply::Empty);
        };
        if let Some(waiters) = self.quarantining.get_mut(&key) {
            waiters.push(req);
            return InitAction::Wait;
        }
        if let Some(pending) = self.pending_reads.get_mut(&key) {
            pending.waiters.push(req);
            return InitAction::Wait;
        }
        self.pending_reads.insert(
            key.clone(),
            PendingRead { waiters: vec![req], attempts_failed: 0, retry_due: None },
        );
        let uri = roots.file_uri(key.shared, &key.extension_id);
        InitAction::Read { uri, key }
    }

    /// Finish an in-flight storage read.
    ///
    /// * Valid JSON or absent file: caches the result (preserving any newer in-memory value),
    ///   replies to waiting requests, and starts any write queued behind the read.
    /// * Corrupt JSON: ALWAYS moves the file aside to `*.corrupt.<millis>-<seq>` first, even if a
    ///   racing `$setValue` or broadcast already populated the cache.
    /// * Transient/fs `Err`: retries up to [`MAX_READ_RETRIES`] times with backoff before marking
    ///   `read_failed`, dropping any queued write, and rejecting waiting requests.
    pub fn on_read_done(
        &mut self,
        now: Instant,
        shared: bool,
        extension_id: &str,
        result: Result<Option<Vec<u8>>, String>,
        next_token: impl FnOnce() -> Token,
    ) -> ReadOutcome {
        let key = StorageKey::new(shared, extension_id);
        let pending = self
            .pending_reads
            .remove(&key)
            .unwrap_or(PendingRead { waiters: Vec::new(), attempts_failed: 0, retry_due: None });
        match result {
            Ok(Some(bytes)) => match decode_persisted_bytes(&bytes) {
                Ok(loaded) => {
                    self.read_failed.remove(&key);
                    let effective = match self.cache.get(&key).cloned() {
                        Some(existing) => existing,
                        None => {
                            self.cache.insert(key.clone(), loaded.clone());
                            loaded
                        }
                    };
                    let reply = reply_for_entry(effective.as_deref());
                    let write = if !self.in_flight_writes.contains_key(&key) && !self.quarantining.contains_key(&key) {
                        self.queued_writes
                            .remove(&key)
                            .and_then(|b| self.start_write(key, b, 0, next_token))
                    } else {
                        None
                    };
                    ReadOutcome {
                        replies: pending.waiters.into_iter().map(|req| (req, Ok(reply.clone()))).collect(),
                        quarantine: None,
                        write,
                    }
                }
                Err(reason) => {
                    tracing::warn!(
                        "ember-editor: corrupt extension storage for {} (shared={}): {reason}; moving aside to .corrupt",
                        key.extension_id,
                        key.shared
                    );
                    if let Some(roots) = &self.roots {
                        let from = roots.file_uri(key.shared, &key.extension_id);
                        let to = corrupt_backup_uri(&from);
                        self.quarantining.entry(key.clone()).or_default().extend(pending.waiters);
                        ReadOutcome { replies: Vec::new(), quarantine: Some((from, to, key)), write: None }
                    } else {
                        let effective = self.cache.entry(key).or_insert(None).clone();
                        let reply = reply_for_entry(effective.as_deref());
                        ReadOutcome {
                            replies: pending.waiters.into_iter().map(|req| (req, Ok(reply.clone()))).collect(),
                            quarantine: None,
                            write: None,
                        }
                    }
                }
            },
            Ok(None) => {
                self.read_failed.remove(&key);
                let effective = match self.cache.get(&key).cloned() {
                    Some(existing) => existing,
                    None => {
                        self.cache.insert(key.clone(), None);
                        None
                    }
                };
                let reply = reply_for_entry(effective.as_deref());
                let write = if !self.in_flight_writes.contains_key(&key) && !self.quarantining.contains_key(&key) {
                    self.queued_writes
                        .remove(&key)
                        .and_then(|b| self.start_write(key, b, 0, next_token))
                } else {
                    None
                };
                ReadOutcome {
                    replies: pending.waiters.into_iter().map(|req| (req, Ok(reply.clone()))).collect(),
                    quarantine: None,
                    write,
                }
            }
            Err(e) => {
                let failed = pending.attempts_failed + 1;
                if failed <= MAX_READ_RETRIES {
                    tracing::warn!(
                        "ember-editor: cannot read extension storage for {} (shared={}) (attempt {}/{}): {e}; retrying",
                        key.extension_id,
                        key.shared,
                        failed,
                        MAX_READ_RETRIES + 1
                    );
                    self.pending_reads.insert(
                        key,
                        PendingRead {
                            waiters: pending.waiters,
                            attempts_failed: failed,
                            retry_due: Some(now + retry_delay(failed)),
                        },
                    );
                    return ReadOutcome::default();
                }
                tracing::warn!(
                    "ember-editor: cannot read extension storage for {} (shared={}) after {} attempts: {e}",
                    key.extension_id,
                    key.shared,
                    failed
                );
                self.read_failed.insert(key.clone());
                self.cache.remove(&key);
                self.queued_writes.remove(&key);
                let err = Some(js_error("Error", &e));
                ReadOutcome {
                    replies: pending.waiters.into_iter().map(|req| (req, Err(err.clone()))).collect(),
                    quarantine: None,
                    write: None,
                }
            }
        }
    }

    /// Finish moving a corrupt storage file aside to `*.corrupt.<millis>-<seq>`.
    pub fn on_quarantine_done(
        &mut self,
        shared: bool,
        extension_id: &str,
        result: Result<(), String>,
        next_token: impl FnOnce() -> Token,
    ) -> QuarantineOutcome {
        let key = StorageKey::new(shared, extension_id);
        let waiters = self.quarantining.remove(&key).unwrap_or_default();
        match result {
            Ok(()) => {
                self.read_failed.remove(&key);
                let val = self.cache.entry(key.clone()).or_insert(None).clone();
                let reply = reply_for_entry(val.as_deref());
                let replies = waiters.into_iter().map(|req| (req, Ok(reply.clone()))).collect();
                let write = self
                    .queued_writes
                    .remove(&key)
                    .and_then(|bytes| self.start_write(key, bytes, 0, next_token));
                QuarantineOutcome { replies, write }
            }
            Err(e) => {
                tracing::warn!(
                    "ember-editor: cannot move corrupt extension storage for {} (shared={}) aside: {e}",
                    key.extension_id,
                    key.shared
                );
                self.read_failed.insert(key.clone());
                self.cache.remove(&key);
                self.queued_writes.remove(&key);
                let err = Some(js_error("Error", &e));
                let replies = waiters.into_iter().map(|req| (req, Err(err.clone()))).collect();
                QuarantineOutcome { replies, write: None }
            }
        }
    }

    /// Handle `MainThreadStorage.$setValue(shared, extensionId, value)`.
    ///
    /// Updates the in-memory cache and enqueues a serialized disk write (coalescing if a read,
    /// write, or quarantine is already in flight). The caller answers `$setValue` immediately
    /// without waiting on disk I/O.
    pub fn on_set_value(
        &mut self,
        shared: bool,
        extension_id: &str,
        value: Value,
        next_token: impl FnOnce() -> Token,
    ) -> SetOutcome {
        let key = StorageKey::new(shared, extension_id);
        if shared {
            self.register_shared_extension(&key.extension_id);
        }
        if self.read_failed.contains(&key) {
            tracing::warn!(
                "ember-editor: refusing to overwrite unreadable extension storage for {} (shared={}) until a read succeeds",
                key.extension_id,
                key.shared
            );
            return SetOutcome::default();
        }
        let (entry, bytes) = if value.is_null() {
            (None, None)
        } else {
            match serde_json::to_string(&value) {
                Ok(raw) => {
                    let b = raw.as_bytes().to_vec();
                    (Some(raw), Some(b))
                }
                Err(e) => {
                    tracing::warn!(
                        "ember-editor: cannot serialize extension storage for {} (shared={}): {e}",
                        key.extension_id,
                        key.shared
                    );
                    return SetOutcome::default();
                }
            }
        };
        self.bump_generation(&key);
        self.cache.insert(key.clone(), entry.clone());
        let broadcast = if key.shared { Some((key.extension_id.clone(), entry)) } else { None };
        if self.roots.is_none() {
            return SetOutcome { write: None, broadcast };
        }
        // A newer value supersedes any pending retry for this key.
        self.retry_writes.remove(&key);
        if self.in_flight_writes.contains_key(&key)
            || self.quarantining.contains_key(&key)
            || self.pending_reads.contains_key(&key)
        {
            self.queued_writes.insert(key, bytes);
            return SetOutcome { write: None, broadcast };
        }
        let write = self.start_write(key, bytes, 0, next_token);
        SetOutcome { write, broadcast }
    }

    /// Finish an in-flight storage write. Starts the latest queued write if one arrived while this
    /// write was in flight; otherwise schedules a bounded retry if the write failed (unless
    /// superseded by an external shared change).
    pub fn on_write_done(
        &mut self,
        now: Instant,
        token: Token,
        result: Result<(), String>,
        next_token: impl FnOnce() -> Token,
    ) -> Option<WriteCommand> {
        let key = self.write_tokens.remove(&token)?;
        let in_flight = self.in_flight_writes.remove(&key)?;
        if let Err(e) = &result {
            tracing::warn!(
                "ember-editor: cannot persist extension storage for {} (shared={}) (attempt {}/{}): {e}",
                key.extension_id,
                key.shared,
                in_flight.attempt + 1,
                MAX_WRITE_RETRIES
            );
        }
        // If a newer value arrived while this write was in flight, write it next regardless of
        // whether the superseded write succeeded or failed.
        if let Some(next_bytes) = self.queued_writes.remove(&key) {
            return self.start_write(key, next_bytes, 0, next_token);
        }
        if result.is_err() && !in_flight.superseded_by_external {
            let next_attempt = in_flight.attempt + 1;
            if next_attempt < MAX_WRITE_RETRIES {
                let due = now + retry_delay(next_attempt);
                self.retry_writes.insert(key, ScheduledRetry { bytes: in_flight.bytes, attempt: next_attempt, due });
            } else {
                tracing::error!(
                    "ember-editor: giving up persisting extension storage for {} (shared={}) after {} attempts",
                    key.extension_id,
                    key.shared,
                    MAX_WRITE_RETRIES
                );
            }
        }
        None
    }

    /// Fire any failed storage writes or reads whose retry backoff has elapsed.
    pub fn on_tick(&mut self, now: Instant, mut next_token: impl FnMut() -> Token) -> TickOutcome {
        let due_write_keys: Vec<StorageKey> = self
            .retry_writes
            .iter()
            .filter(|(_, r)| r.due <= now)
            .map(|(k, _)| k.clone())
            .collect();
        let mut writes = Vec::new();
        for key in due_write_keys {
            if let Some(retry) = self.retry_writes.remove(&key) {
                if let Some(cmd) = self.start_write(key, retry.bytes, retry.attempt, &mut next_token) {
                    writes.push(cmd);
                }
            }
        }

        let mut reads = Vec::new();
        if let Some(roots) = &self.roots {
            for (key, pending) in &mut self.pending_reads {
                if pending.retry_due.is_some_and(|due| due <= now) {
                    pending.retry_due = None;
                    let uri = roots.file_uri(key.shared, &key.extension_id);
                    reads.push(ReadCommand { uri, key: key.clone() });
                }
            }
        }

        TickOutcome { writes, reads }
    }

    /// Apply a `shared = true` (`globalState`) change broadcast by another session or reloaded
    /// from disk. Cancels any pending local retry or queued write so an older local blob cannot
    /// overwrite the newer external value.
    pub fn on_external_shared_change(&mut self, extension_id: &str, value: Option<String>) {
        let key = StorageKey::new(true, extension_id);
        self.register_shared_extension(&key.extension_id);
        self.read_failed.remove(&key);
        self.retry_writes.remove(&key);
        self.queued_writes.remove(&key);
        if let Some(in_flight) = self.in_flight_writes.get_mut(&key) {
            in_flight.superseded_by_external = true;
        }
        self.bump_generation(&key);
        self.cache.insert(key, value);
    }

    /// Apply the result of reloading a `shared = true` file after a directory watch event or hub
    /// lag. Drops replies whose `generation` no longer matches the key's current generation (a
    /// newer local `$setValue` or external update landed while the reload was in flight). Returns
    /// `Some(new_value)` only when the disk content differs from the current cache (filtering
    /// self-echoes and already-delivered hub updates).
    pub fn on_shared_reload_done(
        &mut self,
        extension_id: &str,
        generation: u64,
        result: Result<Option<Vec<u8>>, String>,
    ) -> Option<Option<String>> {
        let key = StorageKey::new(true, extension_id);
        if generation != self.generation_of(&key) {
            return None;
        }
        if self.in_flight_writes.contains_key(&key)
            || self.queued_writes.contains_key(&key)
            || self.quarantining.contains_key(&key)
        {
            return None;
        }
        let loaded = match result {
            Ok(Some(bytes)) => match decode_persisted_bytes(&bytes) {
                Ok(v) => v,
                Err(reason) => {
                    tracing::warn!(
                        "ember-editor: ignoring corrupt external shared storage reload for {}: {reason}",
                        key.extension_id
                    );
                    return None;
                }
            },
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(
                    "ember-editor: cannot reload external shared storage for {}: {e}",
                    key.extension_id
                );
                return None;
            }
        };
        if self.cache.get(&key) == Some(&loaded) {
            return None;
        }
        self.on_external_shared_change(&key.extension_id, loaded.clone());
        Some(loaded)
    }

    /// Handle `MainThreadStorage.$registerExtensionStorageKeysToSync(extension, keys)`.
    pub fn on_register_sync_keys(&mut self, extension: &Value, keys: Vec<String>) {
        let id = extension
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| extension.as_str());
        if let Some(id) = id {
            self.sync_keys.insert(id.to_ascii_lowercase(), keys);
        }
    }

    fn start_write(
        &mut self,
        key: StorageKey,
        bytes: Option<Vec<u8>>,
        attempt: u32,
        next_token: impl FnOnce() -> Token,
    ) -> Option<WriteCommand> {
        let roots = self.roots.as_ref()?;
        let token = next_token();
        let uri = roots.file_uri(key.shared, &key.extension_id);
        self.in_flight_writes.insert(
            key.clone(),
            InFlightWrite { bytes: bytes.clone(), attempt, superseded_by_external: false },
        );
        self.write_tokens.insert(token, key);
        Some(WriteCommand { uri, bytes, token })
    }
}

fn reply_for_entry(entry: Option<&str>) -> Reply {
    match entry {
        Some(raw) => Reply::Json(Value::String(raw.to_owned())),
        None => Reply::Empty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_encoding_blocks_path_traversal_and_separates_collisions() {
        let evil_ids = [
            "../../etc/passwd",
            "..\\..\\windows\\system32",
            "a/b/c",
            ".",
            "..",
            "",
            "foo\0bar",
            "...leading.and.trailing...",
            &"x".repeat(300),
        ];
        for id in evil_ids {
            let encoded = encode_extension_key(id);
            assert!(!encoded.is_empty(), "empty key for {id:?}");
            assert!(!encoded.contains('/'), "slash in {encoded}");
            assert!(!encoded.contains('\\'), "backslash in {encoded}");
            assert!(!encoded.contains(".."), "dot-dot in {encoded}");
            assert!(!encoded.contains('\0'), "nul in {encoded}");
            assert!(!encoded.starts_with('.'), "leading dot in {encoded}");
            assert!(encoded.len() < 128, "too long: {}", encoded.len());
        }

        // Case-insensitive extension ids map to the same key.
        assert_eq!(encode_extension_key("Publisher.MyExt"), encode_extension_key("publisher.myext"));
        // Distinct ids with identical sanitized prefixes stay distinct via the hash suffix.
        assert_ne!(encode_extension_key("a/b"), encode_extension_key("a_b"));

        // Distinct workspace paths with the same folder name get distinct keys.
        assert_ne!(
            encode_workspace_key("host:8000", "/work/one/app"),
            encode_workspace_key("host:8000", "/work/two/app")
        );
        // Same workspace path on different authorities gets distinct keys.
        assert_ne!(
            encode_workspace_key("server-a:8000", "/work/one/app"),
            encode_workspace_key("server-b:8000", "/work/one/app")
        );
        // Authority comparison is case-insensitive and normalizes loopback spellings.
        assert_eq!(
            encode_workspace_key("Server-A:8000", "/work/one/app"),
            encode_workspace_key("server-a:8000", "/work/one/app")
        );
        assert_eq!(canonicalize_authority("localhost:8000"), "localhost:8000");
        assert_eq!(canonicalize_authority("127.0.0.1:8000"), "localhost:8000");
        assert_eq!(canonicalize_authority("[::1]:8000"), "localhost:8000");
        assert_eq!(canonicalize_authority("LOCALHOST"), "localhost");
        assert_eq!(
            encode_workspace_key("localhost:8000", "/work/one/app"),
            encode_workspace_key("127.0.0.1:8000", "/work/one/app")
        );
        assert_eq!(
            encode_workspace_key("localhost:8000", "/work/one/app"),
            encode_workspace_key("[::1]:8000", "/work/one/app")
        );
        // Remote mode workspace_dir ignores endpoint spelling and port.
        let r_a = StorageRoots::from_env(
            "localhost:8000",
            &UriComponents::remote("localhost:8000", "/data/globalStorage"),
            &UriComponents::remote("localhost:8000", "/data/workspaceStorage"),
            "/work/one/app",
        );
        let r_b = StorageRoots::from_env(
            "127.0.0.1:9999",
            &UriComponents::remote("127.0.0.1:9999", "/data/globalStorage"),
            &UriComponents::remote("127.0.0.1:9999", "/data/workspaceStorage"),
            "/work/one/app/",
        );
        assert_eq!(r_a.workspace_dir.path, r_b.workspace_dir.path);
        // Trailing slash normalization keeps the same workspace key.
        assert_eq!(
            encode_workspace_key("host:8000", "/work/one/app"),
            encode_workspace_key("host:8000", "/work/one/app/")
        );
        // Multi-root workspaces get distinct identities from single-root and different folder sets.
        assert_ne!(
            encode_workspace_identity("host:8000", &["/work/a", "/work/b"]),
            encode_workspace_identity("host:8000", &["/work/a"])
        );
        assert_ne!(
            encode_workspace_identity("host:8000", &["/work/a", "/work/b"]),
            encode_workspace_identity("host:8000", &["/work/a/work/b"])
        );
        // Traversal in workspace path cannot escape.
        let ws = encode_workspace_key("host:8000", "/../../../etc/shadow");
        assert!(!ws.contains('/') && !ws.contains(".."));
    }

    #[test]
    fn decode_persisted_bytes_handles_valid_null_empty_and_corrupt() {
        assert_eq!(decode_persisted_bytes(br#"{"a":1}"#).unwrap(), Some(r#"{"a":1}"#.to_owned()));
        assert_eq!(decode_persisted_bytes(b"null").unwrap(), None);
        assert_eq!(decode_persisted_bytes(b"   \n").unwrap(), None);
        assert!(decode_persisted_bytes(b"{not json").is_err());
        assert!(decode_persisted_bytes(&[0xff, 0xfe]).is_err());
    }

    #[test]
    fn local_atomic_write_read_overwrite_and_clear() {
        let dir = std::env::temp_dir().join(format!("ember-storage-unit-{}", uuid::Uuid::new_v4().simple()));
        let file = dir.join("global").join(format!("{}.json", encode_extension_key("pub.ext")));

        assert_eq!(read_local_optional(&file).unwrap(), None);

        write_local_atomic(&file, Some(br#"{"v":1}"#)).unwrap();
        assert_eq!(read_local_optional(&file).unwrap(), Some(br#"{"v":1}"#.to_vec()));

        write_local_atomic(&file, Some(br#"{"v":2,"k":"ok"}"#)).unwrap();
        assert_eq!(read_local_optional(&file).unwrap(), Some(br#"{"v":2,"k":"ok"}"#.to_vec()));

        // No leftover temp files in the directory.
        let entries: Vec<String> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries.len(), 1, "unexpected temp files: {entries:?}");

        // Clear deletes the file cleanly; clearing again is idempotent.
        write_local_atomic(&file, None).unwrap();
        assert_eq!(read_local_optional(&file).unwrap(), None);
        write_local_atomic(&file, None).unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_machine_coalesces_reads_writes_quarantines_corrupt_and_retries_errors() {
        let t0 = Instant::now();
        let mut store = ExtensionStorage::default();
        store.set_roots(StorageRoots::from_env(
            "127.0.0.1:8000",
            &UriComponents::remote("127.0.0.1:8000", "/data/globalStorage"),
            &UriComponents::remote("127.0.0.1:8000", "/data/workspaceStorage"),
            "/work/proj",
        ));

        // Two concurrent initializes for the same key issue only one read.
        let act1 = store.on_initialize(1, true, "Pub.Ext");
        assert!(matches!(act1, InitAction::Read { .. }));
        assert_eq!(store.on_initialize(2, true, "pub.ext"), InitAction::Wait);

        // A `$setValue` arrives while the read is still in flight: cache updates and broadcast
        // fires immediately, while the disk write waits behind the in-flight read so a corrupt or
        // unreadable file on disk is never clobbered before the read finishes.
        let mut tok = 10u64;
        let out1 = store.on_set_value(true, "pub.ext", json!({"v": 1}), || {
            tok += 1;
            tok
        });
        assert!(out1.write.is_none());
        assert!(out1.broadcast.is_some());

        // Overlapping `$setValue` calls while the read is in flight coalesce to the latest value.
        let out2 = store.on_set_value(true, "pub.ext", json!({"v": 2}), || {
            tok += 1;
            tok
        });
        assert!(out2.write.is_none());
        let out3 = store.on_set_value(true, "pub.ext", json!({"v": 3}), || {
            tok += 1;
            tok
        });
        assert!(out3.write.is_none());

        // The stale disk read finishes: it keeps the newer `$setValue` and launches the coalesced
        // write for v=3 (skipping v=1 and v=2).
        let read_out = store.on_read_done(t0, true, "pub.ext", Ok(Some(br#"{"stale":true}"#.to_vec())), || {
            tok += 1;
            tok
        });
        assert!(read_out.quarantine.is_none());
        assert_eq!(
            read_out.replies,
            vec![
                (1, Ok(Reply::Json(Value::String(r#"{"v":3}"#.to_owned())))),
                (2, Ok(Reply::Json(Value::String(r#"{"v":3}"#.to_owned())))),
            ]
        );
        assert_eq!(store.cached(true, "pub.ext"), Some(Some(r#"{"v":3}"#)));
        let started = read_out.write.expect("queued write should start once read finishes");
        assert_eq!(started.token, 11);
        assert_eq!(started.bytes.as_deref(), Some(br#"{"v":3}"#.as_slice()));

        // Overlapping `$setValue` calls while write 11 is in flight coalesce to v=5 (skipping v=4).
        assert!(store.on_set_value(true, "pub.ext", json!({"v": 4}), || { tok += 1; tok }).write.is_none());
        assert!(store.on_set_value(true, "pub.ext", json!({"v": 5}), || { tok += 1; tok }).write.is_none());

        // Completing write 11 launches the coalesced write for v=5.
        let next = store
            .on_write_done(t0, 11, Ok(()), || {
                tok += 1;
                tok
            })
            .expect("queued write should start");
        assert_eq!(next.token, 12);
        assert_eq!(next.bytes.as_deref(), Some(br#"{"v":5}"#.as_slice()));

        // A transient failure on write 12 schedules a retry.
        assert!(store
            .on_write_done(t0, 12, Err("EIO".into()), || {
                tok += 1;
                tok
            })
            .is_none());
        assert!(store.has_pending_writes());
        let wake = store.next_wake().expect("retry deadline scheduled");
        let tick = store.on_tick(wake, || {
            tok += 1;
            tok
        });
        assert_eq!(tick.writes.len(), 1);
        assert_eq!(tick.writes[0].bytes.as_deref(), Some(br#"{"v":5}"#.as_slice()));
        assert!(store
            .on_write_done(wake, tick.writes[0].token, Ok(()), || {
                tok += 1;
                tok
            })
            .is_none());
        assert!(!store.has_pending_writes());

        // Corrupt read triggers quarantine to `*.corrupt.<millis>-<seq>` before resolving as Empty.
        assert!(matches!(store.on_initialize(4, false, "pub.bad"), InitAction::Read { .. }));
        let bad_out = store.on_read_done(t0, false, "pub.bad", Ok(Some(b"{corrupt".to_vec())), || {
            tok += 1;
            tok
        });
        assert!(bad_out.replies.is_empty());
        let (from, to, _) = bad_out.quarantine.expect("quarantine expected");
        assert!(to.path.starts_with(&format!("{}.corrupt.", from.path)), "unexpected backup path: {}", to.path);
        let q_out = store.on_quarantine_done(false, "pub.bad", Ok(()), || {
            tok += 1;
            tok
        });
        assert_eq!(q_out.replies, vec![(4, Ok(Reply::Empty))]);
        assert_eq!(store.cached(false, "pub.bad"), Some(None));

        // Transient read errors retry MAX_READ_RETRIES times with backoff before rejecting,
        // never caching None, and blocking overwrites once exhausted.
        assert!(matches!(store.on_initialize(5, false, "pub.err"), InitAction::Read { .. }));
        let mut now = t0;
        for _ in 0..MAX_READ_RETRIES {
            let err_out = store.on_read_done(now, false, "pub.err", Err("EIO".into()), || {
                tok += 1;
                tok
            });
            assert!(err_out.replies.is_empty(), "transient read error should retry before rejecting");
            let wake = store.next_wake().expect("read retry deadline scheduled");
            now = wake;
            let tick = store.on_tick(now, || {
                tok += 1;
                tok
            });
            assert_eq!(tick.reads.len(), 1);
        }
        let err_out = store.on_read_done(now, false, "pub.err", Err("EIO".into()), || {
            tok += 1;
            tok
        });
        assert_eq!(err_out.replies.len(), 1);
        assert!(err_out.replies[0].1.is_err());
        assert_eq!(store.cached(false, "pub.err"), None);
        let blocked = store.on_set_value(false, "pub.err", json!({"clobber": true}), || {
            tok += 1;
            tok
        });
        assert!(blocked.write.is_none());
        assert_eq!(store.cached(false, "pub.err"), None);

        // Sync keys registration is recorded by lowercased id.
        store.on_register_sync_keys(&json!({"id": "Pub.Ext", "version": "1.0.0"}), vec!["k1".into(), "k2".into()]);
        assert_eq!(store.sync_keys("pub.ext"), Some(&["k1".to_owned(), "k2".to_owned()][..]));
    }

    #[test]
    fn shared_reload_drops_stale_generation_after_local_or_external_update() {
        let mut store = ExtensionStorage::default();
        let roots = StorageRoots::from_env(
            "127.0.0.1:8000",
            &UriComponents::remote("127.0.0.1:8000", "/data/globalStorage"),
            &UriComponents::remote("127.0.0.1:8000", "/data/workspaceStorage"),
            "/work/proj",
        );
        let path = roots.file_uri(true, "pub.gen").path;
        store.set_roots(roots);
        store.register_shared_extension("pub.gen");

        // Snapshot generation before a newer local `$setValue` completes.
        let (_, _, stale_gen) = store.shared_extension_for_path(&path).expect("tracked shared path");
        let mut tok = 1u64;
        let out = store.on_set_value(true, "pub.gen", json!({"v": "local"}), || {
            tok += 1;
            tok
        });
        let write = out.write.expect("local write starts");
        assert!(store.on_write_done(Instant::now(), write.token, Ok(()), || { tok += 1; tok }).is_none());

        // Stale reload reply with `stale_gen` must be dropped even though no write is in flight.
        assert_eq!(
            store.on_shared_reload_done("pub.gen", stale_gen, Ok(Some(br#"{"v":"stale"}"#.to_vec()))),
            None
        );
        assert_eq!(store.cached(true, "pub.gen"), Some(Some(r#"{"v":"local"}"#)));

        // Snapshot current generation, apply an external hub change, then deliver the stale reload.
        let (_, _, stale_gen2) = store.shared_extension_for_path(&path).expect("tracked shared path");
        store.on_external_shared_change("pub.gen", Some(r#"{"v":"hub"}"#.to_owned()));
        assert_eq!(
            store.on_shared_reload_done("pub.gen", stale_gen2, Ok(Some(br#"{"v":"stale2"}"#.to_vec()))),
            None
        );
        assert_eq!(store.cached(true, "pub.gen"), Some(Some(r#"{"v":"hub"}"#)));

        // A reload carrying the current generation applies cleanly and bumps generation again.
        let (_, _, current_gen) = store.shared_extension_for_path(&path).expect("tracked shared path");
        assert_eq!(
            store.on_shared_reload_done("pub.gen", current_gen, Ok(Some(br#"{"v":"disk"}"#.to_vec()))),
            Some(Some(r#"{"v":"disk"}"#.to_owned()))
        );
        assert_eq!(store.cached(true, "pub.gen"), Some(Some(r#"{"v":"disk"}"#)));
        // A second concurrent reload from the same `current_gen` is now stale and dropped.
        assert_eq!(
            store.on_shared_reload_done("pub.gen", current_gen, Ok(Some(br#"{"v":"earlier-disk"}"#.to_vec()))),
            None
        );
        assert_eq!(store.cached(true, "pub.gen"), Some(Some(r#"{"v":"disk"}"#)));
    }
}
