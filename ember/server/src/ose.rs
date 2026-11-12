//! Manages local OSE server processes for editor launches.
//!
//! Each computer has its own OSE data dir, `<data root>/<computer id>`, passed as
//! `--server-data-dir` to every OSE started for it and to every extension install, so an
//! extension is installed once per computer and shared by that computer's folders (SPEC
//! FR-W4, #459). The data root is `EMBER_OSE_DATA_DIR`, else `<EMBER_DATA_DIR or ~/.ember>/ose-data`.
//! **Unknown:** how several OSE processes behave on one shared data dir (no lock or race
//! documentation was found), so nothing here claims it is safe.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::api::ide::EmberOseConnection;

struct Process {
    child: Option<Child>,
    launch_id: String,
    address: SocketAddr,
    real_token: String,
    placeholder: String,
    connection: EmberOseConnection,
    active_relays: Arc<AtomicUsize>,
    /// Removed when the entry goes away (child exited, manager dropped, start failed).
    _token_file: Option<TokenFile>,
}

/// Target and credentials resolved for one active OSE relay stream (`FR-W4`, `#458`, `#461`).
pub struct RelayTarget {
    pub address: SocketAddr,
    pub real_token: String,
    pub placeholder: String,
    pub idle_timeout: Duration,
    _permit: RelayPermit,
}

/// RAII guard for one active OSE relay stream: decrements the per-launch and per-server counters on drop.
struct RelayPermit {
    launch: Arc<AtomicUsize>,
    server: Arc<AtomicUsize>,
}

impl Drop for RelayPermit {
    fn drop(&mut self) {
        self.launch.fetch_sub(1, Ordering::SeqCst);
        self.server.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Why a relay stream could not be opened before the WebSocket upgrade.
#[derive(Debug, PartialEq, Eq)]
pub enum RelayLookupError {
    /// 404: no live launch with this `(computer_id, launch_id)`.
    NotFound,
    /// 429: per-launch or per-server relay concurrency limit reached.
    TooManyRelays,
}

/// The connection token file of one OSE process. The file is deleted on drop.
struct TokenFile(PathBuf);

impl Drop for TokenFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Writes `token` to `<tokens dir>/<first 16 hex of SHA-256 of the folder>.token`, mode 0600
/// from creation (`create_new`), without a trailing newline. A stale file at the path is removed
/// first; a directory there is refused. The token never exists with wider permissions.
fn write_token_file(tokens_dir: &Path, folder: &Path, token: &str) -> std::io::Result<TokenFile> {
    use sha2::{Digest, Sha256};
    use std::io::{Error, ErrorKind, Write};
    // The tokens dir and its parent (the computer dir) hold or lead to a secret: they are
    // hardened, not just created (#521).
    if let Some(parent) = tokens_dir.parent() {
        secure_token_dir(parent)?;
    }
    secure_token_dir(tokens_dir)?;
    let digest = Sha256::digest(folder.as_os_str().as_encoded_bytes());
    let name: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let path = tokens_dir.join(format!("{name}.token"));
    match std::fs::symlink_metadata(&path) {
        Ok(md) if md.is_dir() => {
            return Err(Error::new(ErrorKind::InvalidInput, format!("{} is a directory", path.display())));
        }
        Ok(_) => std::fs::remove_file(&path)?,
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    let guard = TokenFile(path);
    file.write_all(token.as_bytes())?;
    Ok(guard)
}

impl Drop for Process {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The id of the computer this server runs on.
pub const LOCAL_COMPUTER: &str = "local";
/// Longest wait for one `--install-extension` run.
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);
/// The most captured output returned for one install (stdout then stderr).
pub const OUTPUT_CAP: usize = 16 * 1024;
/// Default idle timeout for one relayed OSE connection (`FR-W4`, `#458`).
pub const RELAY_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Maximum concurrent relays per OSE launch (`FR-W4`, `#458`).
pub const MAX_RELAYS_PER_LAUNCH: usize = 16;
/// Maximum concurrent relays across the server (`FR-W4`, `#458`).
pub const MAX_RELAYS_PER_SERVER: usize = 64;

/// The server CLI flags this module passes. `--server-data-dir`, `--install-extension` and
/// `--extensions-dir` are documented in `ember/ose/README.md` and used by `ember/ose/smoke.sh`.
/// `--accept-server-license-terms` is used by every start and by smoke.sh's install step.
const FLAG_DATA_DIR: &str = "--server-data-dir";
const FLAG_EXTENSIONS_DIR: &str = "--extensions-dir";
const FLAG_INSTALL: &str = "--install-extension";
/// UNVERIFIED: `--list-extensions` is not documented or used anywhere in this repository, so no
/// list route exists. Kept as the one place the flag would be named.
#[allow(dead_code)]
const FLAG_LIST_UNVERIFIED: &str = "--list-extensions";

/// Owns one server per (computer, local project folder).
pub struct OseManager {
    binary: Option<PathBuf>,
    /// `Err` when no root can be resolved (no override and no HOME): every use answers the message.
    data_root: Result<PathBuf, String>,
    install_timeout: Duration,
    relay_idle_timeout: Duration,
    max_relays_per_launch: usize,
    max_relays_per_server: usize,
    active_relays: Arc<AtomicUsize>,
    installing: Arc<Mutex<HashSet<String>>>,
    processes: Arc<Mutex<HashMap<(String, PathBuf), Process>>>,
}

/// Why an install did not happen.
#[derive(Debug, PartialEq)]
pub enum InstallError {
    /// 400
    Bad(String),
    /// 503
    NotInstalled,
    /// 409
    Busy,
    /// 502: the command ran and failed or timed out; `output` is what it printed.
    Failed { message: String, output: String },
    /// 500
    Io(String),
}

/// True for an id that is safe as one path segment: only `[A-Za-z0-9._-]`, not empty, no `..`,
/// not `.`, at most 64 bytes.
pub fn valid_computer_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id != "."
        && !id.contains("..")
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn slug_ok(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b[0].is_ascii_alphanumeric()
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
}

/// Checks `publisher.name` (`^[A-Za-z0-9][A-Za-z0-9-]*\.[A-Za-z0-9][A-Za-z0-9-]*$`, at most 128 bytes) and an
/// optional `major.minor.patch[-+suffix]` version, and returns the argument for
/// `--install-extension` (`publisher.name` or `publisher.name@version`). Paths, URLs and
/// anything starting with `-` fail the pattern.
pub fn extension_spec(id: &str, version: Option<&str>) -> Result<String, String> {
    let ok_id = id.len() <= 128
        && matches!(id.split_once('.'), Some((p, n)) if slug_ok(p) && slug_ok(n));
    if !ok_id {
        return Err("extension id must look like publisher.name (letters, digits, '-')".into());
    }
    let Some(v) = version else { return Ok(id.to_string()) };
    let (core, rest) = match v.find(['-', '+']) {
        Some(i) => (&v[..i], Some(&v[i + 1..])),
        None => (v, None),
    };
    let nums: Vec<&str> = core.split('.').collect();
    let nums_ok = nums.len() == 3
        && nums.iter().all(|n| !n.is_empty() && n.len() <= 9 && n.bytes().all(|b| b.is_ascii_digit()));
    let rest_ok = rest.map_or(true, |r| {
        !r.is_empty() && r.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    });
    if v.len() > 64 || !nums_ok || !rest_ok {
        return Err("version must look like 1.2.3 or 1.2.3-pre.1".into());
    }
    Ok(format!("{id}@{v}"))
}

/// The OSE data root from the environment values (kept pure so tests need no env). With no
/// `EMBER_OSE_DATA_DIR`, no `EMBER_DATA_DIR` and no usable HOME it is an error: a relative path
/// under the working directory is never used.
pub fn data_root_from(
    ose_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Result<PathBuf, String> {
    let non_empty = |p: Option<PathBuf>| p.filter(|p| !p.as_os_str().is_empty());
    if let Some(p) = non_empty(ose_dir) {
        return Ok(p);
    }
    let base = match non_empty(data_dir) {
        Some(d) => d,
        None => non_empty(home)
            .ok_or_else(|| {
                "cannot resolve the OSE data dir: set EMBER_OSE_DATA_DIR or EMBER_DATA_DIR, or HOME".to_string()
            })?
            .join(".ember"),
    };
    Ok(base.join("ose-data"))
}

fn default_data_root() -> Result<PathBuf, String> {
    data_root_from(
        std::env::var_os("EMBER_OSE_DATA_DIR").map(PathBuf::from),
        std::env::var_os("EMBER_DATA_DIR").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

/// Makes `path` (and missing parents) with mode 0700 and returns it ready for use.
///
/// A symlink at `path` is refused, as is anything that is not a directory. A directory that
/// already exists is used as it is: its mode is NOT changed (it may be a directory the user chose
/// with `EMBER_OSE_DATA_DIR`, even a shared one), a warning is logged when group or others can
/// access it, and it must be owned by the current user. Only a directory this call creates is
/// set to 0700.
fn make_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::{Error, ErrorKind};
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        match std::fs::symlink_metadata(path) {
            Ok(md) => {
                if md.file_type().is_symlink() {
                    return Err(Error::new(ErrorKind::PermissionDenied, format!("{} is a symlink", path.display())));
                }
                if !md.is_dir() {
                    return Err(Error::new(ErrorKind::InvalidInput, format!("{} is not a directory", path.display())));
                }
                // SAFETY: geteuid takes no arguments and cannot fail.
                if md.uid() != unsafe { libc::geteuid() } {
                    return Err(Error::new(
                        ErrorKind::PermissionDenied,
                        format!("{} is not owned by the current user", path.display()),
                    ));
                }
                if md.permissions().mode() & 0o077 != 0 {
                    tracing::warn!(
                        path = %path.display(),
                        mode = format!("{:o}", md.permissions().mode() & 0o777),
                        "OSE data dir is accessible to group or others; its mode is left unchanged"
                    );
                }
                Ok(())
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path)?;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            }
            Err(e) => Err(e),
        }
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

/// Makes `path` a directory only this user can use, for a directory that holds or leads to a
/// connection token file (the tokens dir and the computer dir above it).
///
/// Created with 0700 when missing. A symlink, a non-directory or a directory owned by another
/// user is refused. An existing directory that group or others can WRITE (`mode & 0o022`) is
/// refused with `token directory <path> is writable by others; fix the permissions`: another user
/// could delete and replace the 0600 token file before the OSE reads it. An existing directory
/// with only group/other read or search bits (`0o055`) is tightened to 0700, because we own it and
/// a token file lives there; this is logged at warn. The shared data root above is not touched
/// here (see [`make_private_dir`]).
fn secure_token_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::{Error, ErrorKind};
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        match std::fs::symlink_metadata(path) {
            Ok(md) => {
                if md.file_type().is_symlink() {
                    return Err(Error::new(ErrorKind::PermissionDenied, format!("{} is a symlink", path.display())));
                }
                if !md.is_dir() {
                    return Err(Error::new(ErrorKind::InvalidInput, format!("{} is not a directory", path.display())));
                }
                // SAFETY: geteuid takes no arguments and cannot fail.
                if md.uid() != unsafe { libc::geteuid() } {
                    return Err(Error::new(
                        ErrorKind::PermissionDenied,
                        format!("{} is not owned by the current user", path.display()),
                    ));
                }
                let mode = md.permissions().mode() & 0o777;
                if mode & 0o022 != 0 {
                    return Err(Error::new(
                        ErrorKind::PermissionDenied,
                        format!("token directory {} is writable by others; fix the permissions", path.display()),
                    ));
                }
                if mode & 0o077 != 0 {
                    tracing::warn!(
                        path = %path.display(),
                        mode = format!("{mode:o}"),
                        "token directory is readable by group or others; tightening it to 0700"
                    );
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
                }
                Ok(())
            }
            Err(e) if e.kind() == ErrorKind::NotFound => make_private_dir(path),
            Err(e) => Err(e),
        }
    }
    #[cfg(not(unix))]
    {
        make_private_dir(path)
    }
}

/// The variables an OSE child gets from the server's environment. Everything else (server tokens,
/// other secrets) is dropped. `ember/ose/smoke.sh` and `regress.sh` set no variable of their own
/// and call the launcher with the caller's environment, so nothing beyond this list is known to
/// be needed. Not forwarded, to be added only when proven needed: proxy variables.
const ENV_ALLOWLIST: [&str; 6] = ["HOME", "PATH", "LANG", "LC_ALL", "TMPDIR", "USER"];

/// A command for the OSE binary with a cleared environment plus the allowlist.
fn ose_command(binary: &Path) -> Command {
    let mut cmd = Command::new(binary);
    cmd.env_clear();
    for key in ENV_ALLOWLIST {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    cmd
}

/// Kills a child and everything in its process group (the child leads its own group, see
/// `process_group(0)` at the spawn), then reaps it.
fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        // SAFETY: killpg takes plain integers; a stale group id at worst fails with ESRCH.
        unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Removes the computer from the busy set when dropped.
struct BusyGuard {
    set: Arc<Mutex<HashSet<String>>>,
    id: String,
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.set.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.id);
    }
}

/// Reads a pipe to its end on a thread, keeping at most `OUTPUT_CAP` bytes.
fn capture<R: Read + Send + 'static>(mut r: R) -> (Arc<Mutex<Vec<u8>>>, Arc<AtomicBool>) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(AtomicBool::new(false));
    let (b, d) = (buf.clone(), done.clone());
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(n) = r.read(&mut chunk) {
            if n == 0 {
                break;
            }
            let mut b = b.lock().unwrap_or_else(|e| e.into_inner());
            let room = OUTPUT_CAP.saturating_sub(b.len());
            b.extend_from_slice(&chunk[..n.min(room)]);
        }
        d.store(true, Ordering::SeqCst);
    });
    (buf, done)
}

fn collect(parts: &[(Arc<Mutex<Vec<u8>>>, Arc<AtomicBool>)], wait: bool) -> String {
    if wait {
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until && !parts.iter().all(|(_, d)| d.load(Ordering::SeqCst)) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let mut all = Vec::new();
    for (b, _) in parts {
        all.extend_from_slice(&b.lock().unwrap_or_else(|e| e.into_inner()));
    }
    let mut text = String::from_utf8_lossy(&all).into_owned();
    if text.len() > OUTPUT_CAP {
        let mut end = OUTPUT_CAP;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

impl Default for OseManager {
    fn default() -> Self {
        Self::new(server_binary())
    }
}

impl OseManager {
    pub fn new(binary: Option<PathBuf>) -> Self {
        Self::with_root(binary, default_data_root())
    }

    pub fn with_data_root(binary: Option<PathBuf>, data_root: PathBuf) -> Self {
        Self::with_root(binary, Ok(data_root))
    }

    fn with_root(binary: Option<PathBuf>, data_root: Result<PathBuf, String>) -> Self {
        let processes: Arc<Mutex<HashMap<(String, PathBuf), Process>>> = Arc::new(Mutex::new(HashMap::new()));
        let weak = Arc::downgrade(&processes);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            let Some(processes) = weak.upgrade() else { break };
            let mut processes = processes.lock().unwrap_or_else(|e| e.into_inner());
            processes.retain(|_, process| match &mut process.child {
                Some(child) => matches!(child.try_wait(), Ok(None)),
                None => true,
            });
        });
        Self {
            binary,
            data_root,
            install_timeout: INSTALL_TIMEOUT,
            relay_idle_timeout: RELAY_IDLE_TIMEOUT,
            max_relays_per_launch: MAX_RELAYS_PER_LAUNCH,
            max_relays_per_server: MAX_RELAYS_PER_SERVER,
            active_relays: Arc::new(AtomicUsize::new(0)),
            installing: Arc::new(Mutex::new(HashSet::new())),
            processes,
        }
    }

    /// Overrides the install timeout (tests).
    pub fn with_install_timeout(mut self, timeout: Duration) -> Self {
        self.install_timeout = timeout;
        self
    }

    /// Overrides the relay idle timeout (tests).
    pub fn with_relay_idle_timeout(mut self, timeout: Duration) -> Self {
        self.relay_idle_timeout = timeout;
        self
    }

    /// Overrides the per-launch and per-server relay concurrency limits (tests).
    pub fn with_relay_limits(mut self, per_launch: usize, per_server: usize) -> Self {
        self.max_relays_per_launch = per_launch;
        self.max_relays_per_server = per_server;
        self
    }

    /// Registers an existing OSE TCP listener for `(computer_id, folder)` without spawning a
    /// child process (used by relay integration tests).
    pub fn register_launch_for_test(
        &self,
        computer_id: &str,
        folder: &Path,
        address: SocketAddr,
        real_token: &str,
    ) -> EmberOseConnection {
        let launch_id = uuid::Uuid::new_v4().simple().to_string();
        let mut placeholder = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        while placeholder == real_token {
            placeholder = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        }
        let relay = format!("/api/computers/{computer_id}/ide/relay/{launch_id}");
        let connection = EmberOseConnection {
            endpoint: format!("http://{address}"),
            relay: Some(relay),
            connection_token: placeholder.clone(),
        };
        let folder = folder.canonicalize().unwrap_or_else(|_| folder.to_path_buf());
        let key = (computer_id.to_string(), folder);
        let mut processes = self.processes.lock().unwrap_or_else(|e| e.into_inner());
        processes.insert(
            key,
            Process {
                child: None,
                launch_id,
                address,
                real_token: real_token.to_string(),
                placeholder,
                connection: connection.clone(),
                active_relays: Arc::new(AtomicUsize::new(0)),
                _token_file: None,
            },
        );
        connection
    }

    /// Resolves `(computer_id, launch_id)` and acquires a concurrency permit for one relay stream.
    pub fn open_relay(&self, computer_id: &str, launch_id: &str) -> Result<RelayTarget, RelayLookupError> {
        let mut processes = self.processes.lock().unwrap_or_else(|e| e.into_inner());
        let mut found = None;
        for ((comp, _), p) in processes.iter_mut() {
            if comp == computer_id
                && p.launch_id == launch_id
                && match &mut p.child {
                    Some(c) => matches!(c.try_wait(), Ok(None)),
                    None => true,
                }
            {
                found = Some(p);
                break;
            }
        }
        let Some(process) = found else {
            return Err(RelayLookupError::NotFound);
        };
        let launch_active = process.active_relays.load(Ordering::SeqCst);
        let server_active = self.active_relays.load(Ordering::SeqCst);
        if launch_active >= self.max_relays_per_launch || server_active >= self.max_relays_per_server {
            return Err(RelayLookupError::TooManyRelays);
        }
        process.active_relays.fetch_add(1, Ordering::SeqCst);
        self.active_relays.fetch_add(1, Ordering::SeqCst);
        let permit = RelayPermit {
            launch: process.active_relays.clone(),
            server: self.active_relays.clone(),
        };
        Ok(RelayTarget {
            address: process.address,
            real_token: process.real_token.clone(),
            placeholder: process.placeholder.clone(),
            idle_timeout: self.relay_idle_timeout,
            _permit: permit,
        })
    }

    /// Removes the `*.token` files a killed server left behind (#521): every regular file named
    /// `<hex>.token` in `<data root>/<computer id>/tokens/`. Call it once at startup, before any OSE
    /// start: a SIGKILL skips the [`TokenFile`] drop, so the dead token stays on disk until the
    /// same folder starts again. Symlinks and directories are left alone, a missing root is not an
    /// error, and an OSE of ANOTHER server sharing the same data root loses its file too (it read
    /// the file at its start, see FR-W4). Returns how many files were removed.
    pub fn sweep_stale_tokens(&self) -> usize {
        let Ok(root) = self.data_root() else { return 0 };
        let Ok(computers) = std::fs::read_dir(root) else { return 0 };
        let mut removed = 0;
        for computer in computers.flatten() {
            let name = computer.file_name();
            if !valid_computer_id(&name.to_string_lossy()) {
                continue;
            }
            let tokens = computer.path().join("tokens");
            match std::fs::symlink_metadata(&tokens) {
                Ok(md) if md.is_dir() => {}
                _ => continue,
            }
            let Ok(files) = std::fs::read_dir(&tokens) else { continue };
            for file in files.flatten() {
                let path = file.path();
                let is_token = path.extension().is_some_and(|e| e == "token")
                    && path.file_stem().is_some_and(|s| {
                        let s = s.to_string_lossy();
                        !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
                    });
                let regular = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file());
                if is_token && regular && std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }
        if removed > 0 {
            tracing::info!(removed, "removed stale OSE token files left by an earlier server");
        }
        removed
    }

    pub fn data_root(&self) -> Result<&Path, String> {
        self.data_root.as_deref().map_err(|e| e.clone())
    }

    /// `<data root>/<computer id>`, created with mode 0700. The id is validated first, so a
    /// request value can never leave the root.
    pub fn computer_data_dir(&self, computer_id: &str) -> Result<PathBuf, String> {
        if !valid_computer_id(computer_id) {
            return Err(format!("invalid computer id: {computer_id:?}"));
        }
        let root = self.data_root()?;
        make_private_dir(root).map_err(|e| format!("cannot create OSE data root: {e}"))?;
        let dir = root.join(computer_id);
        make_private_dir(&dir).map_err(|e| format!("cannot create OSE data dir: {e}"))?;
        Ok(dir)
    }

    fn dir_args(dir: &Path) -> Vec<std::ffi::OsString> {
        vec![
            FLAG_DATA_DIR.into(),
            dir.as_os_str().to_owned(),
            FLAG_EXTENSIONS_DIR.into(),
            dir.join("extensions").into_os_string(),
        ]
    }

    /// Runs `<binary> --install-extension <id>[@version]` against the computer's data dir. No
    /// shell; the arguments are separate argv entries; one install at a time per computer; the
    /// child runs with a cleared environment (allowlist only) in its own process group, which is
    /// killed on timeout. A server shutdown or panic does not kill a running install (TODO follow-up). Blocking: call from `spawn_blocking`.
    pub fn install_extension(
        &self,
        computer_id: &str,
        id: &str,
        version: Option<&str>,
    ) -> Result<String, InstallError> {
        if !valid_computer_id(computer_id) {
            return Err(InstallError::Bad(format!("invalid computer id: {computer_id:?}")));
        }
        let spec = extension_spec(id, version).map_err(InstallError::Bad)?;
        let binary = self.binary.as_ref().filter(|p| p.is_file()).ok_or(InstallError::NotInstalled)?;
        {
            let mut busy = self.installing.lock().unwrap_or_else(|e| e.into_inner());
            if !busy.insert(computer_id.to_string()) {
                return Err(InstallError::Busy);
            }
        }
        let _guard = BusyGuard { set: self.installing.clone(), id: computer_id.to_string() };
        let dir = self.computer_data_dir(computer_id).map_err(InstallError::Io)?;
        let mut command = ose_command(binary);
        command
            .arg(FLAG_INSTALL)
            .arg(&spec)
            .args(Self::dir_args(&dir))
            .arg("--accept-server-license-terms")
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        // Own process group, so a timeout kills a launcher's children too.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command.spawn().map_err(|e| InstallError::Io(format!("cannot start OSE server: {e}")))?;
        let parts = [capture(child.stdout.take().expect("piped")), capture(child.stderr.take().expect("piped"))];
        let deadline = Instant::now() + self.install_timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let output = collect(&parts, true);
                    return if status.success() {
                        Ok(output)
                    } else {
                        Err(InstallError::Failed { message: format!("install failed: {status}"), output })
                    };
                }
                Ok(None) if Instant::now() >= deadline => {
                    kill_tree(&mut child);
                    let output = collect(&parts, false);
                    return Err(InstallError::Failed {
                        message: format!("install timed out after {}s", self.install_timeout.as_secs_f32()),
                        output,
                    });
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => {
                    kill_tree(&mut child);
                    return Err(InstallError::Io(format!("cannot wait for the OSE server: {e}")));
                }
            }
        }
    }

    /// The local computer's server for `folder`.
    pub fn connection(&self, folder: &Path) -> Result<EmberOseConnection, String> {
        self.connection_for(LOCAL_COMPUTER, folder)
    }

    /// The server of `computer_id` for `folder`, started with that computer's data dir.
    pub fn connection_for(&self, computer_id: &str, folder: &Path) -> Result<EmberOseConnection, String> {
        let folder = folder.canonicalize().map_err(|e| format!("cannot open project folder: {e}"))?;
        let key = (computer_id.to_string(), folder.clone());
        let mut processes = self.processes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(process) = processes.get_mut(&key) {
            let alive = match &mut process.child {
                Some(child) => matches!(child.try_wait(), Ok(None)),
                None => true,
            };
            if alive {
                return Ok(process.connection.clone());
            }
        }
        processes.remove(&key);
        let binary = self.binary.as_ref().filter(|p| p.is_file())
            .ok_or_else(|| "OSE server binary is not installed".to_string())?;
        let data_dir = self.computer_data_dir(computer_id)?;
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("cannot allocate OSE port: {e}"))?;
        let address = listener.local_addr().map_err(|e| format!("cannot read OSE port: {e}"))?;
        drop(listener);
        let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let mut placeholder = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        while placeholder == token {
            placeholder = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        }
        let launch_id = uuid::Uuid::new_v4().simple().to_string();
        let token_file = write_token_file(&data_dir.join("tokens"), &folder, &token)
            .map_err(|e| format!("cannot write the OSE token file: {e}"))?;
        let child = ose_command(binary)
            .args(["--host", "127.0.0.1", "--port", &address.port().to_string(), "--connection-token-file"])
            .arg(&token_file.0)
            .args(["--accept-server-license-terms", "--telemetry-level", "off"])
            .args(Self::dir_args(&data_dir))
            .current_dir(&folder)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().map_err(|e| format!("cannot start OSE server: {e}"))?;
        let connection = EmberOseConnection {
            endpoint: format!("http://{address}"),
            relay: Some(format!("/api/computers/{computer_id}/ide/relay/{launch_id}")),
            connection_token: placeholder.clone(),
        };
        let mut process = Process {
            child: Some(child),
            launch_id,
            address,
            real_token: token,
            placeholder,
            connection: connection.clone(),
            active_relays: Arc::new(AtomicUsize::new(0)),
            _token_file: Some(token_file),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(child) = &mut process.child {
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(format!("OSE server exited during startup: {status}"));
                }
            }
            if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() { break; }
            if Instant::now() >= deadline { return Err("OSE server did not start in time".into()); }
            std::thread::sleep(Duration::from_millis(50));
        }
        processes.insert(key, process);
        Ok(connection)
    }
}

fn server_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("EMBER_OSE_SERVER").filter(|v| !v.is_empty()) {
        return Some(path.into());
    }
    let share = PathBuf::from(std::env::var_os("HOME")?).join(".local/share");
    let mut candidates: Vec<_> = std::fs::read_dir(share).ok()?.flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("dpx-ose-"))
        .map(|e| e.path().join("bin/dpx-ose-server")).filter(|p| p.is_file()).collect();
    candidates.sort();
    candidates.pop()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FR-W4: the per-computer busy flag is released on every path out of an install, including
    /// a panic. (A dropped HTTP future does not cancel the blocking task: `spawn_blocking` keeps
    /// running to its end, and the guard drops there.)
    #[test]
    fn fr_w4_ose_data_dir_busy_guard_releases_on_panic_and_drop() {
        let set = Arc::new(Mutex::new(HashSet::new()));
        set.lock().unwrap().insert("local".to_string());
        let s = set.clone();
        let result = std::panic::catch_unwind(move || {
            let _guard = BusyGuard { set: s, id: "local".into() };
            panic!("install blew up");
        });
        assert!(result.is_err());
        assert!(set.lock().unwrap_or_else(|e| e.into_inner()).is_empty(), "guard must release on panic");
        set.lock().unwrap_or_else(|e| e.into_inner()).insert("local".to_string());
        drop(BusyGuard { set: set.clone(), id: "local".into() });
        assert!(set.lock().unwrap().is_empty());
    }
}
