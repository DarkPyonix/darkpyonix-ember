//! Secret keys in the OS keystore, with a file fallback (SPEC FR-S13, #784).
//!
//! `secret.key` (server), `transport.key` (server and node) and any other 32-byte device key go
//! to the OS keystore where one is available: the macOS Keychain, the Windows Credential Manager
//! (DPAPI-protected) or a Linux Secret Service. Without one, the key stays in its file (mode
//! 0600), as before. The residual risk of the file fallback is written in the install guide.
//!
//! # Mode
//!
//! `EMBER_KEYSTORE` picks the store: `file` (always the file), `os` (the OS keystore, refusing to
//! start without one) or `auto` (the default: the OS keystore when it answers a probe, otherwise
//! the file). The mode is read only by the binaries ([`install_from_env`]); a library caller
//! that installs nothing gets the file, so tests never touch a real keystore.
//!
//! # Migration
//!
//! On first start with an existing key file and an available keystore, the key is copied into
//! the keystore, read back and compared, and only then is the file deleted and a
//! `<file>.migrated` marker written. If the write or the read-back fails, the file stays and is
//! used: a key is never lost.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Context;

/// The keystore service name every Ember entry uses.
pub const SERVICE: &str = "dev.darkpyonix.ember";

/// A place that holds named secrets.
pub trait KeyStore: Send + Sync {
    /// Whether the store answers at all (a probe, not a key read).
    fn available(&self) -> bool;
    /// The secret under `account`, `None` if there is none.
    fn get(&self, account: &str) -> anyhow::Result<Option<Vec<u8>>>;
    /// Store `secret` under `account`, replacing any previous value.
    fn set(&self, account: &str, secret: &[u8]) -> anyhow::Result<()>;
}

/// The store picked by `EMBER_KEYSTORE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    File,
    Os,
    Auto,
}

impl Mode {
    /// Parse `EMBER_KEYSTORE`; unset or empty is `auto`.
    pub fn parse(value: Option<&str>) -> anyhow::Result<Mode> {
        match value.map(str::trim) {
            None | Some("") | Some("auto") => Ok(Mode::Auto),
            Some("file") => Ok(Mode::File),
            Some("os") => Ok(Mode::Os),
            Some(other) => anyhow::bail!("EMBER_KEYSTORE must be file, os or auto, not {other:?}"),
        }
    }

    pub fn from_env() -> anyhow::Result<Mode> {
        Mode::parse(std::env::var("EMBER_KEYSTORE").ok().as_deref())
    }
}

/// The store to use for `mode`, given the OS store: `None` means the file.
pub fn choose(mode: Mode, os: Arc<dyn KeyStore>) -> anyhow::Result<Option<Arc<dyn KeyStore>>> {
    match mode {
        Mode::File => Ok(None),
        Mode::Os => {
            anyhow::ensure!(os.available(), "EMBER_KEYSTORE=os but no OS keystore answers");
            Ok(Some(os))
        }
        Mode::Auto => Ok(os.available().then_some(os)),
    }
}

static INSTALLED: OnceLock<Option<Arc<dyn KeyStore>>> = OnceLock::new();

/// Read `EMBER_KEYSTORE`, probe the OS keystore and install the result for this process.
/// Called once at the top of a binary's `main`; a second call is a no-op.
pub fn install_from_env() -> anyhow::Result<()> {
    let chosen = choose(Mode::from_env()?, Arc::new(OsKeyStore))?;
    match &chosen {
        Some(_) => tracing::info!("keys: OS keystore"),
        None => tracing::info!("keys: files (no OS keystore, or EMBER_KEYSTORE=file)"),
    }
    let _ = INSTALLED.set(chosen);
    Ok(())
}

/// The installed store, `None` (the file) when nothing was installed.
pub fn installed() -> Option<Arc<dyn KeyStore>> {
    INSTALLED.get().cloned().flatten()
}

/// The keystore account for the key file `path`: its file name and canonical directory, so two
/// data dirs on one computer never share a key.
pub fn account_for(path: &Path) -> String {
    let dir = path.parent().unwrap_or(Path::new("."));
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    format!("{name}@{}", dir.display())
}

/// Where a key came from.
#[derive(Debug, PartialEq, Eq)]
pub enum Resolved {
    /// The keystore holds it (found, migrated from the file or freshly generated).
    Keystore(Vec<u8>),
    /// Use the file: no store, or the store failed and the file is kept.
    File,
}

/// The marker left beside a migrated key file.
pub fn migrated_marker(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".migrated");
    path.with_file_name(name)
}

/// Resolve the key for the file `path` against `store`.
///
/// `read_file` is the caller's existing file loader (it validates length and mode) and
/// `generate` makes a fresh key. With no store, or on any store failure, this returns
/// [`Resolved::File`] and the caller loads or creates the file as before.
pub fn resolve(
    store: Option<&dyn KeyStore>,
    path: &Path,
    read_file: impl FnOnce(&Path) -> anyhow::Result<Vec<u8>>,
    generate: impl FnOnce() -> Vec<u8>,
) -> anyhow::Result<Resolved> {
    let Some(store) = store else { return Ok(Resolved::File) };
    let account = account_for(path);
    let in_store = match store.get(&account) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("keystore read of {account} failed, using the file: {e:#}");
            return Ok(Resolved::File);
        }
    };
    if let Some(key) = in_store {
        if path.exists() {
            tracing::warn!(
                "{} exists beside a keystore entry; the keystore wins, the file is left for you to remove",
                path.display()
            );
        }
        return Ok(Resolved::Keystore(key));
    }
    let (key, from_file) = if path.exists() {
        (read_file(path).with_context(|| format!("reading {}", path.display()))?, true)
    } else {
        (generate(), false)
    };
    let stored = store
        .set(&account, &key)
        .and_then(|()| store.get(&account))
        .map(|back| back.as_deref() == Some(key.as_slice()));
    match stored {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!("keystore read-back of {account} did not match; keeping the file");
            return Ok(Resolved::File);
        }
        Err(e) => {
            tracing::warn!("keystore write of {account} failed, keeping the file: {e:#}");
            return Ok(Resolved::File);
        }
    }
    if from_file {
        let marker = migrated_marker(path);
        let note = format!("moved to the OS keystore ({SERVICE} / {account})\n");
        std::fs::write(&marker, note).with_context(|| format!("writing {}", marker.display()))?;
        std::fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
        tracing::info!("moved {} to the OS keystore", path.display());
    }
    Ok(Resolved::Keystore(key))
}

/// The OS keystore through the `keyring` crate (Keychain, Credential Manager, Secret Service).
pub struct OsKeyStore;

impl KeyStore for OsKeyStore {
    fn available(&self) -> bool {
        match keyring::Entry::new(SERVICE, "probe").and_then(|e| e.get_secret()) {
            Ok(_) | Err(keyring::Error::NoEntry) => true,
            Err(e) => {
                tracing::debug!("OS keystore unavailable: {e}");
                false
            }
        }
    }

    fn get(&self, account: &str) -> anyhow::Result<Option<Vec<u8>>> {
        match keyring::Entry::new(SERVICE, account)?.get_secret() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn set(&self, account: &str, secret: &[u8]) -> anyhow::Result<()> {
        Ok(keyring::Entry::new(SERVICE, account)?.set_secret(secret)?)
    }
}

/// An in-memory store for tests, with switches to simulate failures.
#[derive(Default)]
pub struct MemoryKeyStore {
    pub entries: Mutex<std::collections::HashMap<String, Vec<u8>>>,
    /// `available()` answers false.
    pub unavailable: bool,
    /// `set` fails.
    pub fail_set: bool,
    /// `set` stores a different value, so the read-back does not match.
    pub corrupt_set: bool,
}

impl KeyStore for MemoryKeyStore {
    fn available(&self) -> bool {
        !self.unavailable
    }

    fn get(&self, account: &str) -> anyhow::Result<Option<Vec<u8>>> {
        anyhow::ensure!(!self.unavailable, "memory store unavailable");
        Ok(self.entries.lock().unwrap().get(account).cloned())
    }

    fn set(&self, account: &str, secret: &[u8]) -> anyhow::Result<()> {
        anyhow::ensure!(!self.unavailable && !self.fail_set, "memory store write failed");
        let mut value = secret.to_vec();
        if self.corrupt_set {
            value.iter_mut().for_each(|b| *b ^= 0xff);
        }
        self.entries.lock().unwrap().insert(account.to_owned(), value);
        Ok(())
    }
}

/// Load the transport key for the file `path` (`transport.key`) through the installed store,
/// falling back to [`ember_transport::SecretKey::load_or_generate`] on the file.
pub fn load_transport_key(path: &Path) -> anyhow::Result<ember_transport::SecretKey> {
    load_transport_key_with(installed().as_deref(), path)
}

/// [`load_transport_key`] against an explicit store (tests).
pub fn load_transport_key_with(
    store: Option<&dyn KeyStore>,
    path: &Path,
) -> anyhow::Result<ember_transport::SecretKey> {
    use ember_transport::SecretKey;
    let resolved = resolve(
        store,
        path,
        |p| Ok(SecretKey::load_or_generate(p)?.to_bytes().to_vec()),
        || SecretKey::generate().to_bytes().to_vec(),
    )?;
    match resolved {
        Resolved::Keystore(bytes) => {
            let bytes: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("keystore entry for {} is not 32 bytes", path.display()))?;
            Ok(SecretKey::from_bytes(bytes))
        }
        Resolved::File => Ok(SecretKey::load_or_generate(path)?),
    }
}
