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
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::api::ide::EmberOseConnection;

struct Process {
    child: Child,
    connection: EmberOseConnection,
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The id of the computer this server runs on.
pub const LOCAL_COMPUTER: &str = "local";
/// Longest wait for one `--install-extension` run.
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);
/// The most captured output returned for one install (stdout then stderr).
pub const OUTPUT_CAP: usize = 16 * 1024;

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
    data_root: PathBuf,
    install_timeout: Duration,
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
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// Checks `publisher.name` (`^[a-z0-9][a-z0-9-]*\.[a-z0-9][a-z0-9-]*$`, at most 128 bytes) and an
/// optional `major.minor.patch[-+suffix]` version, and returns the argument for
/// `--install-extension` (`publisher.name` or `publisher.name@version`). Paths, URLs and
/// anything starting with `-` fail the pattern.
pub fn extension_spec(id: &str, version: Option<&str>) -> Result<String, String> {
    let ok_id = id.len() <= 128
        && matches!(id.split_once('.'), Some((p, n)) if slug_ok(p) && slug_ok(n));
    if !ok_id {
        return Err("extension id must look like publisher.name (lowercase letters, digits, '-')".into());
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

/// The OSE data root from the environment values (kept pure so tests need no env).
pub fn data_root_from(ose_dir: Option<PathBuf>, data_dir: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    let non_empty = |p: Option<PathBuf>| p.filter(|p| !p.as_os_str().is_empty());
    if let Some(p) = non_empty(ose_dir) {
        return p;
    }
    let base = non_empty(data_dir).unwrap_or_else(|| home.unwrap_or_default().join(".ember"));
    base.join("ose-data")
}

fn default_data_root() -> PathBuf {
    data_root_from(
        std::env::var_os("EMBER_OSE_DATA_DIR").map(PathBuf::from),
        std::env::var_os("EMBER_DATA_DIR").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

fn make_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
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
        Self::with_data_root(binary, default_data_root())
    }

    pub fn with_data_root(binary: Option<PathBuf>, data_root: PathBuf) -> Self {
        let processes: Arc<Mutex<HashMap<(String, PathBuf), Process>>> = Arc::new(Mutex::new(HashMap::new()));
        let weak = Arc::downgrade(&processes);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            let Some(processes) = weak.upgrade() else { break };
            let mut processes = processes.lock().unwrap_or_else(|e| e.into_inner());
            processes.retain(|_, process| matches!(process.child.try_wait(), Ok(None)));
        });
        Self {
            binary,
            data_root,
            install_timeout: INSTALL_TIMEOUT,
            installing: Arc::new(Mutex::new(HashSet::new())),
            processes,
        }
    }

    /// Overrides the install timeout (tests).
    pub fn with_install_timeout(mut self, timeout: Duration) -> Self {
        self.install_timeout = timeout;
        self
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    /// `<data root>/<computer id>`, created with mode 0700. The id is validated first, so a
    /// request value can never leave the root.
    pub fn computer_data_dir(&self, computer_id: &str) -> Result<PathBuf, String> {
        if !valid_computer_id(computer_id) {
            return Err(format!("invalid computer id: {computer_id:?}"));
        }
        make_private_dir(&self.data_root).map_err(|e| format!("cannot create OSE data root: {e}"))?;
        let dir = self.data_root.join(computer_id);
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
    /// child is killed through its handle on timeout. Blocking: call from `spawn_blocking`.
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
        let mut child = Command::new(binary)
            .arg(FLAG_INSTALL)
            .arg(&spec)
            .args(Self::dir_args(&dir))
            .arg("--accept-server-license-terms")
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().map_err(|e| InstallError::Io(format!("cannot start OSE server: {e}")))?;
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
                    let _ = child.kill();
                    let _ = child.wait();
                    let output = collect(&parts, false);
                    return Err(InstallError::Failed {
                        message: format!("install timed out after {}s", self.install_timeout.as_secs_f32()),
                        output,
                    });
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
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
        let binary = self.binary.as_ref().filter(|p| p.is_file())
            .ok_or_else(|| "OSE server binary is not installed".to_string())?;
        let folder = folder.canonicalize().map_err(|e| format!("cannot open project folder: {e}"))?;
        let data_dir = self.computer_data_dir(computer_id)?;
        let key = (computer_id.to_string(), folder.clone());
        let mut processes = self.processes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(process) = processes.get_mut(&key) {
            if matches!(process.child.try_wait(), Ok(None)) {
                return Ok(process.connection.clone());
            }
        }
        processes.remove(&key);
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("cannot allocate OSE port: {e}"))?;
        let address = listener.local_addr().map_err(|e| format!("cannot read OSE port: {e}"))?;
        drop(listener);
        let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let child = Command::new(binary)
            .args(["--host", "127.0.0.1", "--port", &address.port().to_string(), "--connection-token", &token,
                "--accept-server-license-terms", "--telemetry-level", "off"])
            .args(Self::dir_args(&data_dir))
            .current_dir(&folder)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().map_err(|e| format!("cannot start OSE server: {e}"))?;
        let connection = EmberOseConnection { endpoint: format!("http://{address}"), connection_token: token };
        let mut process = Process { child, connection: connection.clone() };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(Some(status)) = process.child.try_wait() {
                return Err(format!("OSE server exited during startup: {status}"));
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
