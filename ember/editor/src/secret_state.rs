//! Secret storage backend for the `MainThreadSecretState` actor
//! (`$getPassword`, `$setPassword`, `$deletePassword`).
//!
//! Desktop builds default to the OS keychain through the `keyring` crate using the same service
//! name as Ember's node and server keystore (`dev.darkpyonix.ember`). Tests inject
//! [`InMemorySecretStore`] via `SessionConfig::with_secret_store` so they never touch the host
//! keychain.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Keychain service name shared with Ember's node and server keystore entries.
pub const KEYCHAIN_SERVICE: &str = "dev.darkpyonix.ember";

/// Encodes `(extension_id, key)` into a collision-free keychain account string under the
/// `editor-secret:` namespace. Prefixing `extension_id` with its byte length guarantees that
/// delimiters inside `extension_id` or `key` can never alias another extension's entries.
pub fn account_key(extension_id: &str, key: &str) -> String {
    format!("editor-secret:{}:{extension_id}:{key}", extension_id.len())
}

/// One secret-state operation issued by [`crate::SessionCore`] to be executed by the session
/// driver against the configured [`SecretStore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretOp {
    Get { extension_id: String, key: String },
    Set { extension_id: String, key: String, value: String },
    Delete { extension_id: String, key: String },
}

/// The successful outcome of a [`SecretOp`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretReply {
    Value(Option<String>),
    /// A write succeeded and the entry changed.
    Unit,
    /// A delete succeeded; `existed` says whether an entry was removed.
    Deleted { existed: bool },
}

/// Storage backend for `MainThreadSecretState`.
///
/// Implementations must be thread-safe (`Send + Sync`) because the session driver executes
/// keychain operations on a blocking worker thread.
pub trait SecretStore: Send + Sync + std::fmt::Debug {
    /// Read the secret for `(extension_id, key)`, returning `Ok(None)` when no entry exists.
    fn get(&self, extension_id: &str, key: &str) -> Result<Option<String>, String>;
    /// Store `value` under `(extension_id, key)`, replacing any previous entry.
    fn set(&self, extension_id: &str, key: &str, value: &str) -> Result<(), String>;
    /// Remove the secret for `(extension_id, key)`. Deleting a missing entry succeeds.
    fn delete(&self, extension_id: &str, key: &str) -> Result<(), String>;
}

/// Run one [`SecretOp`] against `store`.
pub fn execute_op(store: &dyn SecretStore, op: SecretOp) -> Result<SecretReply, String> {
    match op {
        SecretOp::Get { extension_id, key } => store.get(&extension_id, &key).map(SecretReply::Value),
        SecretOp::Set { extension_id, key, value } => store.set(&extension_id, &key, &value).map(|()| SecretReply::Unit),
        SecretOp::Delete { extension_id, key } => {
            let existed = store.get(&extension_id, &key)?.is_some();
            store.delete(&extension_id, &key).map(|()| SecretReply::Deleted { existed })
        }
    }
}

/// Deterministic in-memory [`SecretStore`] for tests and platforms without an OS keychain.
#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    entries: Mutex<HashMap<(String, String), String>>,
}

impl InMemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of currently stored entries, for assertions in tests.
    pub fn snapshot(&self) -> HashMap<(String, String), String> {
        self.entries.lock().unwrap().clone()
    }
}

impl SecretStore for InMemorySecretStore {
    fn get(&self, extension_id: &str, key: &str) -> Result<Option<String>, String> {
        let guard = self.entries.lock().map_err(|_| "secret store lock poisoned".to_owned())?;
        Ok(guard.get(&(extension_id.to_owned(), key.to_owned())).cloned())
    }

    fn set(&self, extension_id: &str, key: &str, value: &str) -> Result<(), String> {
        let mut guard = self.entries.lock().map_err(|_| "secret store lock poisoned".to_owned())?;
        guard.insert((extension_id.to_owned(), key.to_owned()), value.to_owned());
        Ok(())
    }

    fn delete(&self, extension_id: &str, key: &str) -> Result<(), String> {
        let mut guard = self.entries.lock().map_err(|_| "secret store lock poisoned".to_owned())?;
        guard.remove(&(extension_id.to_owned(), key.to_owned()));
        Ok(())
    }
}

/// OS keychain backend via the `keyring` crate (macOS Keychain, Windows Credential Manager,
/// Linux Secret Service).
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Debug, Clone)]
pub struct OsSecretStore {
    service: String,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl Default for OsSecretStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl OsSecretStore {
    pub fn new() -> Self {
        Self { service: KEYCHAIN_SERVICE.to_owned() }
    }

    pub fn with_service(service: impl Into<String>) -> Self {
        Self { service: service.into() }
    }

    /// Probe whether the OS keychain answers on this host without writing any entry.
    pub fn available(&self) -> bool {
        match keyring::Entry::new(&self.service, "editor-secret-probe").and_then(|e| e.get_password()) {
            Ok(_) | Err(keyring::Error::NoEntry) => true,
            Err(e) => {
                tracing::debug!("ember-editor: OS keychain unavailable for MainThreadSecretState: {e}");
                false
            }
        }
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl SecretStore for OsSecretStore {
    fn get(&self, extension_id: &str, key: &str) -> Result<Option<String>, String> {
        let account = account_key(extension_id, key);
        let entry = keyring::Entry::new(&self.service, &account).map_err(|e| e.to_string())?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, extension_id: &str, key: &str, value: &str) -> Result<(), String> {
        let account = account_key(extension_id, key);
        let entry = keyring::Entry::new(&self.service, &account).map_err(|e| e.to_string())?;
        entry.set_password(value).map_err(|e| e.to_string())
    }

    fn delete(&self, extension_id: &str, key: &str) -> Result<(), String> {
        let account = account_key(extension_id, key);
        let entry = keyring::Entry::new(&self.service, &account).map_err(|e| e.to_string())?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Default [`SecretStore`] used when `SessionConfig::secret_store` is `None`.
///
/// Honours `EMBER_KEYSTORE=file` (in-memory fallback without touching the OS keychain). On desktop
/// platforms, probes [`OsSecretStore`] and falls back to [`InMemorySecretStore`] when no keychain
/// is available.
pub fn default_secret_store() -> Arc<dyn SecretStore> {
    if std::env::var("EMBER_KEYSTORE").ok().as_deref().map(str::trim) == Some("file") {
        return Arc::new(InMemorySecretStore::new());
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let os = OsSecretStore::new();
        if os.available() {
            return Arc::new(os);
        }
        tracing::info!("ember-editor: OS keychain unavailable; using in-memory secret store");
    }
    Arc::new(InMemorySecretStore::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_key_is_collision_free_across_delimiters() {
        assert_ne!(account_key("a:b", "c"), account_key("a", "b:c"));
        assert_ne!(account_key("ext.one", "token"), account_key("ext.two", "token"));
        assert!(account_key("ext.one", "token").starts_with("editor-secret:"));
    }

    #[test]
    fn in_memory_store_round_trips_deletes_and_isolates_extensions() {
        let store = InMemorySecretStore::new();
        assert_eq!(store.get("ext.a", "apiKey").unwrap(), None);

        store.set("ext.a", "apiKey", "secret-1").unwrap();
        store.set("ext.b", "apiKey", "secret-2").unwrap();
        assert_eq!(store.get("ext.a", "apiKey").unwrap().as_deref(), Some("secret-1"));
        assert_eq!(store.get("ext.b", "apiKey").unwrap().as_deref(), Some("secret-2"));

        store.set("ext.a", "apiKey", "secret-1-updated").unwrap();
        assert_eq!(store.get("ext.a", "apiKey").unwrap().as_deref(), Some("secret-1-updated"));
        assert_eq!(store.get("ext.b", "apiKey").unwrap().as_deref(), Some("secret-2"));

        store.delete("ext.a", "apiKey").unwrap();
        assert_eq!(store.get("ext.a", "apiKey").unwrap(), None);
        assert_eq!(store.get("ext.b", "apiKey").unwrap().as_deref(), Some("secret-2"));

        // Deleting a missing key succeeds.
        store.delete("ext.a", "apiKey").unwrap();
        store.delete("ext.missing", "nope").unwrap();
    }

    #[test]
    fn execute_op_maps_get_set_delete() {
        let store = InMemorySecretStore::new();
        assert_eq!(
            execute_op(&store, SecretOp::Get { extension_id: "e".into(), key: "k".into() }).unwrap(),
            SecretReply::Value(None),
        );
        assert_eq!(
            execute_op(&store, SecretOp::Set { extension_id: "e".into(), key: "k".into(), value: "v".into() }).unwrap(),
            SecretReply::Unit,
        );
        assert_eq!(
            execute_op(&store, SecretOp::Get { extension_id: "e".into(), key: "k".into() }).unwrap(),
            SecretReply::Value(Some("v".into())),
        );
        assert_eq!(
            execute_op(&store, SecretOp::Delete { extension_id: "e".into(), key: "k".into() }).unwrap(),
            SecretReply::Deleted { existed: true },
        );
        assert_eq!(
            execute_op(&store, SecretOp::Get { extension_id: "e".into(), key: "k".into() }).unwrap(),
            SecretReply::Value(None),
        );
    }
}
