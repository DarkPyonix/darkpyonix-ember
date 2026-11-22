//! Persistent storage for `MainThreadStorage` (`globalState` and `workspaceState`).
//!
//! Extensions read their state during activation via `$initializeExtensionStorage(shared, extensionId)`
//! and persist updates via `$setValue(shared, extensionId, value)`.
//!
//! * `shared = true` (`globalState`): one JSON file per extension under the global storage directory.
//! * `shared = false` (`workspaceState`): one JSON file per extension under a per-workspace
//!   subdirectory of the workspace storage directory.
//!
//! Keys are encoded into single safe path components with a deterministic 64-bit hash suffix so
//! arbitrary extension ids or workspace paths can never escape their storage directory or collide
//! on case-insensitive filesystems. Missing or corrupt files are treated gracefully as absent
//! (`Reply::Empty` / JS `undefined`) so extension activation is never blocked.

use std::collections::HashMap;
use std::path::Path;

use ember_editor_conn::rpc::{js_error, Reply};
use ember_editor_conn::uri::UriComponents;
use serde_json::Value;

use crate::engine::Token;

/// Subdirectory under the server's `globalStorageHome` / `workspaceStorageHome` where Ember stores
/// per-extension JSON files, kept separate from Code-OSS's own `state.vscdb` and extension folders.
pub const STORAGE_SUBDIR: &str = "ember-storage";

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

/// Encode a workspace path into a single safe directory name.
///
/// Trailing slashes are stripped (except `"/"` itself) so `/work/app` and `/work/app/` share the
/// same workspace storage. Distinct paths with the same leaf name get distinct hash suffixes.
pub fn encode_workspace_key(workspace_path: &str) -> String {
    let normalized = if workspace_path == "/" {
        "/"
    } else {
        workspace_path.trim_end_matches(['/', '\\'])
    };
    let leaf = normalized
        .rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or("ws");
    let prefix = safe_prefix(leaf, 32, false, "ws");
    format!("{prefix}-{:016x}", fnv1a64(normalized.as_bytes()))
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
        let global_path = join_uri_path(&global_storage_home.path, STORAGE_SUBDIR);
        let ws_key = encode_workspace_key(workspace_path);
        let ws_base = join_uri_path(&workspace_storage_home.path, STORAGE_SUBDIR);
        let ws_path = join_uri_path(&ws_base, &ws_key);
        Self {
            global_dir: UriComponents::remote(authority, &global_path),
            workspace_dir: UriComponents::remote(authority, &ws_path),
        }
    }

    /// Derive storage roots under a local directory on the client machine.
    pub fn from_local_dir(root: &Path, workspace_path: &str) -> Self {
        let global = root.join("global");
        let workspace = root.join("workspace").join(encode_workspace_key(workspace_path));
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
    /// A read for this key is already in flight; `req` has been queued on it.
    Wait,
}

/// What the core must do for a `$setValue` request.
#[derive(Debug, Clone, PartialEq)]
pub enum SetAction {
    /// Answer immediately (in-memory only, no persistence roots configured).
    Reply(Reply),
    /// Persist `bytes` (`None` deletes the file) and answer `req` when [`ExtensionStorage::on_write_done`] runs.
    Write { uri: UriComponents, bytes: Option<Vec<u8>>, token: Token },
}

/// Pure state machine for `MainThreadStorage`: in-memory cache, coalesced reads, pending writes,
/// and registered sync keys.
#[derive(Debug, Default)]
pub struct ExtensionStorage {
    roots: Option<StorageRoots>,
    cache: HashMap<StorageKey, Option<String>>,
    pending_reads: HashMap<StorageKey, Vec<u32>>,
    pending_writes: HashMap<Token, u32>,
    sync_keys: HashMap<String, Vec<String>>,
}

impl ExtensionStorage {
    pub fn set_roots(&mut self, roots: StorageRoots) {
        self.roots = Some(roots);
    }

    pub fn roots(&self) -> Option<&StorageRoots> {
        self.roots.as_ref()
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

    /// Handle `MainThreadStorage.$initializeExtensionStorage(shared, extensionId)`.
    pub fn on_initialize(&mut self, req: u32, shared: bool, extension_id: &str) -> InitAction {
        let key = StorageKey::new(shared, extension_id);
        if let Some(entry) = self.cache.get(&key) {
            return InitAction::Reply(reply_for_entry(entry.as_deref()));
        }
        let Some(roots) = &self.roots else {
            return InitAction::Reply(Reply::Empty);
        };
        let waiters = self.pending_reads.entry(key.clone()).or_default();
        let first = waiters.is_empty();
        waiters.push(req);
        if first {
            let uri = roots.file_uri(key.shared, &key.extension_id);
            InitAction::Read { uri, key }
        } else {
            InitAction::Wait
        }
    }

    /// Finish an in-flight storage read and return `(req, reply)` for every waiting request.
    pub fn on_read_done(
        &mut self,
        shared: bool,
        extension_id: &str,
        result: Result<Option<Vec<u8>>, String>,
    ) -> Vec<(u32, Reply)> {
        let key = StorageKey::new(shared, extension_id);
        let waiters = self.pending_reads.remove(&key).unwrap_or_default();
        // If `$setValue` wrote a value while the read was in flight, keep the newer in-memory value.
        let value = if let Some(existing) = self.cache.get(&key) {
            existing.clone()
        } else {
            let loaded = match result {
                Ok(Some(bytes)) => match decode_persisted_bytes(&bytes) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            "ember-editor: ignoring corrupt extension storage for {} (shared={}): {e}",
                            key.extension_id,
                            key.shared
                        );
                        None
                    }
                },
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(
                        "ember-editor: cannot read extension storage for {} (shared={}): {e}",
                        key.extension_id,
                        key.shared
                    );
                    None
                }
            };
            self.cache.insert(key, loaded.clone());
            loaded
        };
        let reply = reply_for_entry(value.as_deref());
        waiters.into_iter().map(|req| (req, reply.clone())).collect()
    }

    /// Handle `MainThreadStorage.$setValue(shared, extensionId, value)`.
    pub fn on_set_value(
        &mut self,
        req: u32,
        shared: bool,
        extension_id: &str,
        value: Value,
        next_token: impl FnOnce() -> Token,
    ) -> SetAction {
        let key = StorageKey::new(shared, extension_id);
        let (entry, bytes) = if value.is_null() {
            (None, None)
        } else {
            let raw = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_owned());
            let b = raw.as_bytes().to_vec();
            (Some(raw), Some(b))
        };
        self.cache.insert(key.clone(), entry);
        let Some(roots) = &self.roots else {
            return SetAction::Reply(Reply::Empty);
        };
        let token = next_token();
        self.pending_writes.insert(token, req);
        let uri = roots.file_uri(key.shared, &key.extension_id);
        SetAction::Write { uri, bytes, token }
    }

    /// Finish an in-flight storage write and return the response for the waiting `$setValue` request.
    pub fn on_write_done(&mut self, token: Token, result: Result<(), String>) -> Option<(u32, Result<Reply, Option<Value>>)> {
        let req = self.pending_writes.remove(&token)?;
        let reply = match result {
            Ok(()) => Ok(Reply::Empty),
            Err(e) => {
                tracing::warn!("ember-editor: cannot persist extension storage: {e}");
                Err(Some(js_error("Error", &e)))
            }
        };
        Some((req, reply))
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
        assert_ne!(encode_workspace_key("/work/one/app"), encode_workspace_key("/work/two/app"));
        // Trailing slash normalization keeps the same workspace key.
        assert_eq!(encode_workspace_key("/work/one/app"), encode_workspace_key("/work/one/app/"));
        // Traversal in workspace path cannot escape.
        let ws = encode_workspace_key("/../../../etc/shadow");
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
    fn state_machine_coalesces_reads_and_preserves_inflight_set_value() {
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

        // A `$setValue` arrives while the read is still in flight.
        let set_act = store.on_set_value(3, true, "pub.ext", json!({"fresh": true}), || 10);
        assert!(matches!(set_act, SetAction::Write { token: 10, bytes: Some(_), .. }));

        // The stale disk read finishes: it must not overwrite the newer `$setValue`.
        let replies = store.on_read_done(true, "pub.ext", Ok(Some(br#"{"stale":true}"#.to_vec())));
        assert_eq!(
            replies,
            vec![
                (1, Reply::Json(Value::String(r#"{"fresh":true}"#.to_owned()))),
                (2, Reply::Json(Value::String(r#"{"fresh":true}"#.to_owned()))),
            ]
        );
        assert_eq!(store.cached(true, "pub.ext"), Some(Some(r#"{"fresh":true}"#)));

        // Write completion answers req 3 with Empty.
        assert_eq!(store.on_write_done(10, Ok(())), Some((3, Ok(Reply::Empty))));

        // Corrupt read for another extension resolves gracefully as Empty.
        assert!(matches!(store.on_initialize(4, false, "pub.bad"), InitAction::Read { .. }));
        let replies = store.on_read_done(false, "pub.bad", Ok(Some(b"{corrupt".to_vec())));
        assert_eq!(replies, vec![(4, Reply::Empty)]);
        assert_eq!(store.cached(false, "pub.bad"), Some(None));

        // Sync keys registration is recorded by lowercased id.
        store.on_register_sync_keys(&json!({"id": "Pub.Ext", "version": "1.0.0"}), vec!["k1".into(), "k2".into()]);
        assert_eq!(store.sync_keys("pub.ext"), Some(&["k1".to_owned(), "k2".to_owned()][..]));
    }
}
