//! The OSE command line of the server's own machine: extension installs and uninstalls into the
//! per-computer OSE data dir (SPEC FR-W4, FR-L31.14), and the helpers they share.
//!
//! **No OSE is started here** (`INTENT.md` D55, #1064). The editor server of every computer is the
//! DarkPyonix manager's ([`crate::editor_manager`]): the manager installs it and runs it on a 0700
//! Unix socket, and Ember only asks it for a launch ticket and relays bytes to it. What stays here
//! is the server's local extension install and uninstall (`ember_server::ose` re-exports this
//! module), which still runs the OSE binary found by `EMBER_OSE_SERVER` or under
//! `~/.local/share/dpx-ose-*`, until those routes go to the manager too.
//!
//! Each computer has its own OSE data dir, `<data root>/<computer id>`, passed as
//! `--server-data-dir` to every extension command for it (SPEC FR-W4, #459). On the server the
//! data root is `EMBER_OSE_DATA_DIR`, else `<runtime dir>/ose-data`, where the runtime dir is the
//! data dir's sibling ([`runtime_dir`], FR-A11).

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod process;
use process::ManagedChild;

/// The message of an extension command refused because there is no OSE binary on the server's
/// machine. It names the install step (SPEC FR-W4 acceptance step 6, #83).
pub const NOT_INSTALLED: &str = "OSE server binary is not installed: run install-ose.sh from the \
    ember-server or ember-node folder (it fetches the matching DarkPyonix OSE release into \
    ~/.local/share), or set EMBER_OSE_SERVER to a dpx-ose-server launcher";

/// The Code-OSS commit of the OSE build that `binary` launches (SPEC FR-E10, #32): `commit` in the
/// build's `product.json`, which sits beside `bin/` (`<root>/bin/dpx-ose-server` and
/// `<root>/product.json`, the layout of the release archives and of `install-ose.sh`). `None` for
/// a missing or unreadable file, or a value that is not a hex commit of at most 64 characters.
pub fn ose_commit(binary: &Path) -> Option<String> {
    let root = binary.parent()?.parent()?;
    let text = std::fs::read_to_string(root.join("product.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let commit = value.get("commit")?.as_str()?.trim();
    (!commit.is_empty() && commit.len() <= 64 && commit.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| commit.to_ascii_lowercase())
}

/// The runtime dir for the server's `data_dir`: its sibling `<name>-runtime` (SPEC FR-A11). The
/// server's `runtime_dir` module re-exports it and holds the migration.
pub fn runtime_dir(data_dir: &Path) -> PathBuf {
    let name = data_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "ember".into());
    let parent = data_dir.parent().map(Path::to_path_buf).unwrap_or_default();
    parent.join(format!("{name}-runtime"))
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
/// `--uninstall-extension <publisher.name>` (SPEC FR-L31.14, #446). The flag is in Code-OSS
/// 1.139.1 `src/vs/server/node/serverEnvironmentService.ts` (`'uninstall-extension'`), handled by
/// `ExtensionManagementCLI.uninstallExtensions`. UNVERIFIED against a real `dpx-ose-server` run.
const FLAG_UNINSTALL: &str = "--uninstall-extension";
/// `--file-watcher-polling <ms>` (upstream `serverEnvironmentService.ts`): the OSE's recursive
/// watcher polls at this interval instead of waiting for native file events
/// (`remoteFileSystemProviderServer.ts` `getRecursiveWatcherOptions`).
const FLAG_WATCHER_POLLING: &str = "--file-watcher-polling";
/// Polling interval for a project on a network share, in milliseconds: Code-OSS's own default
/// when polling is on (`src/vs/platform/files/common/diskFileSystemProvider.ts` L119, 5000).
pub const NETWORK_WATCH_POLL_MS: u32 = 5000;

/// The watcher flags of an OSE for a project folder on a `kind` filesystem (SPEC FR-L31.24,
/// #789). A local disk, wherever it is mounted, uses native file events and gets no flag. Only a
/// real network filesystem is polled, because native events do not report what another machine
/// changes on a share.
pub fn watcher_args(kind: crate::fs_kind::FsKind) -> Vec<String> {
    match kind {
        crate::fs_kind::FsKind::Local => Vec::new(),
        crate::fs_kind::FsKind::Network => vec![FLAG_WATCHER_POLLING.to_string(), NETWORK_WATCH_POLL_MS.to_string()],
    }
}

/// UNVERIFIED: `--list-extensions` is not documented or used anywhere in this repository, so no
/// list route exists. Kept as the one place the flag would be named.
#[allow(dead_code)]
const FLAG_LIST_UNVERIFIED: &str = "--list-extensions";

/// Runs the OSE binary's extension commands against the per-computer data dirs.
pub struct OseManager {
    binary: Option<PathBuf>,
    /// `Err` when no root can be resolved (no override and no HOME): every use answers the message.
    data_root: Result<PathBuf, String>,
    install_timeout: Duration,
    installing: Arc<Mutex<HashSet<String>>>,
    stopping: AtomicBool,
}

/// What the terminals of an OSE window need to reach persistent sessions (SPEC FR-P5, #26).
/// **[provisional]** VS Code runs tasks through `terminal.integrated.automationProfile` =
/// `ember-term`, and its terminals inherit the OSE server's environment, so:
/// - `ember_term_dir` (the folder holding `ember-term`) goes first on the child's `PATH`;
/// - for the local computer, `local_node_state_dir` becomes `EMBER_NODE_STATE_DIR`, whose
///   `local.json` the built-in node writes at start, so `ember-term` finds that node. No token is
///   put in the environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalEnv {
    pub ember_term_dir: Option<PathBuf>,
    pub local_node_state_dir: Option<PathBuf>,
}

impl TerminalEnv {
    /// The variables to set on the OSE child of `computer_id`, given the child's `PATH` from
    /// the allowlist (`base_path`). Pure, for tests.
    pub fn vars(&self, computer_id: &str, base_path: Option<&std::ffi::OsStr>) -> Vec<(String, std::ffi::OsString)> {
        let mut out = Vec::new();
        if let Some(dir) = &self.ember_term_dir {
            let mut parts = vec![dir.clone()];
            if let Some(p) = base_path {
                parts.extend(std::env::split_paths(p).filter(|x| x != dir));
            }
            if let Ok(joined) = std::env::join_paths(parts) {
                out.push(("PATH".to_string(), joined));
            }
        }
        if computer_id == LOCAL_COMPUTER {
            if let Some(dir) = &self.local_node_state_dir {
                out.push(("EMBER_NODE_STATE_DIR".to_string(), dir.clone().into_os_string()));
            }
        }
        out
    }
}

/// Where `ember-term` is: `EMBER_TERM_BIN` (a file), else next to the server binary (`exe`).
/// `None` leaves `PATH` as it is (an `ember-term` already on it still works).
pub fn find_ember_term_dir(env_bin: Option<std::ffi::OsString>, exe: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(p) = env_bin.filter(|v| !v.is_empty()).map(PathBuf::from) {
        return p.is_file().then(|| p.parent().map(Path::to_path_buf)).flatten();
    }
    let dir = exe?.parent()?.to_path_buf();
    dir.join("ember-term").is_file().then_some(dir)
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
    let rest_ok = rest.is_none_or(|r| {
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
    Ok(runtime_dir(&base).join("ose-data"))
}

fn default_data_root() -> Result<PathBuf, String> {
    data_root_from(
        std::env::var_os("EMBER_OSE_DATA_DIR").map(PathBuf::from),
        std::env::var_os("EMBER_DATA_DIR").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

/// `\\?\C:\x` -> `C:\x` and `\\?\UNC\server\share\x` -> `\\server\share\x`; any other path is
/// unchanged. Pure and platform independent so it is tested everywhere.
pub fn strip_verbatim_prefix(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    match path.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_string(),
        _ => path.to_string(),
    }
}

/// The spelling of an existing directory handed to the OSE. On Windows it is the long form:
/// libuv's fs-event aborts the OSE when a watched directory is named by its 8.3 short form
/// (`C:\Users\RUNNER~1`, FR-W4a, #83). `canonicalize` expands short names (the node has no
/// `windows-sys` dependency for `GetLongPathNameW`); its `\\?\` prefix is stripped. Only the
/// spelling changes: on failure the path is returned as it is. Elsewhere the identity.
pub fn long_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    if let Ok(long) = path.canonicalize() {
        if let Some(s) = long.to_str() {
            return PathBuf::from(strip_verbatim_prefix(s));
        }
    }
    path.to_path_buf()
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

/// The variables an OSE child gets from the server's environment. Everything else (server tokens,
/// other secrets) is dropped. `ember/ose/smoke.sh` and `regress.sh` set no variable of their own
/// and call the launcher with the caller's environment, so nothing beyond this list is known to
/// be needed. More names come only from [`PASSTHROUGH_VAR`] (#471).
const ENV_ALLOWLIST: [&str; 6] = ["HOME", "PATH", "LANG", "LC_ALL", "TMPDIR", "USER"];

/// Added on Windows (#83, D49): Node needs `SystemRoot` for sockets and crypto, and the user and
/// temp folders come from these instead of `HOME` and `TMPDIR`.
const WINDOWS_ENV_ALLOWLIST: [&str; 9] =
    ["SystemRoot", "SystemDrive", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP", "PATHEXT", "COMSPEC"];

/// The variables an OSE child keeps from the parent, before any passthrough.
pub fn env_allowlist(windows: bool) -> Vec<&'static str> {
    let mut names = ENV_ALLOWLIST.to_vec();
    if windows {
        names.extend(WINDOWS_ENV_ALLOWLIST);
    }
    names
}

/// The launcher's file name in `bin/`: `dpx-ose-server`, or `dpx-ose-server.cmd` on Windows.
pub fn launcher_name(windows: bool) -> &'static str {
    if windows { "dpx-ose-server.cmd" } else { "dpx-ose-server" }
}

/// The launcher of the unpacked build at `root`.
pub fn launcher_in(root: &Path, windows: bool) -> PathBuf {
    root.join("bin").join(launcher_name(windows))
}

/// The program and leading arguments that run `binary`. A `.cmd` or `.bat` launcher (Windows,
/// #83, D49) is replaced by what it runs, `<root>/node.exe <root>/out/server-main.js`, so no
/// `cmd.exe` sits between the node and the server: killing the child kills the server, and the
/// arguments are not re-quoted by `cmd.exe`. Any other launcher runs as is. Pure: no file is read.
pub fn launch_program(binary: &Path) -> (PathBuf, Vec<std::ffi::OsString>) {
    let is_batch = binary
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    match binary.parent().and_then(Path::parent) {
        Some(root) if is_batch => {
            (root.join("node.exe"), vec![root.join("out").join("server-main.js").into_os_string()])
        }
        _ => (binary.to_path_buf(), Vec::new()),
    }
}

/// Owner configuration (server environment, never a request field): a comma-separated list of
/// extra variable NAMES passed to the OSE children, default empty, for example
/// `HTTPS_PROXY,NO_PROXY,SSL_CERT_FILE` (FR-W4, #471). **[provisional]**
pub const PASSTHROUGH_VAR: &str = "EMBER_OSE_ENV_PASSTHROUGH";

/// The proxy variables that conventionally exist in lower case too; only these may be listed in
/// lower case.
const LOWER_CASE_PROXY: [&str; 4] = ["http_proxy", "https_proxy", "no_proxy", "all_proxy"];

/// Whether `name` may be passed through: `[A-Z_][A-Z0-9_]*`, or a lower-case proxy name.
pub fn valid_passthrough_name(name: &str) -> bool {
    if LOWER_CASE_PROXY.contains(&name) {
        return true;
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Parses the passthrough list. Empty entries are skipped; invalid names are returned apart so
/// the caller can log them, and are never passed.
pub fn parse_passthrough(raw: &str) -> (Vec<String>, Vec<String>) {
    let (mut ok, mut bad) = (Vec::new(), Vec::new());
    for name in raw.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        if valid_passthrough_name(name) {
            if !ok.iter().any(|n: &String| n == name) {
                ok.push(name.to_string());
            }
        } else {
            bad.push(name.to_string());
        }
    }
    (ok, bad)
}

/// The environment of an OSE child: the allowlist plus the valid passthrough names, each taken
/// from `lookup` when set.
pub fn ose_env(
    passthrough: Option<&str>,
    lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<(String, std::ffi::OsString)> {
    let mut names: Vec<String> = env_allowlist(cfg!(windows)).iter().map(|s| s.to_string()).collect();
    if let Some(raw) = passthrough {
        let (ok, bad) = parse_passthrough(raw);
        for name in bad {
            tracing::warn!(name = %name, "{PASSTHROUGH_VAR}: invalid variable name ignored");
        }
        for name in ok {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names.into_iter().filter_map(|n| lookup(&n).map(|v| (n, v))).collect()
}

/// A command for the OSE binary with a cleared environment plus [`ose_env`].
fn ose_command(binary: &Path) -> Command {
    let passthrough = std::env::var(PASSTHROUGH_VAR).ok();
    ose_command_with(binary, passthrough.as_deref(), |k| std::env::var_os(k))
}

fn ose_command_with(
    binary: &Path,
    passthrough: Option<&str>,
    lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Command {
    let (program, leading) = launch_program(binary);
    let mut cmd = Command::new(program);
    cmd.args(leading);
    cmd.env_clear();
    for (key, value) in ose_env(passthrough, lookup) {
        cmd.env(key, value);
    }
    cmd
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

/// A captured stream: the bytes read so far, and whether the reader reached the end.
type Captured = (Arc<Mutex<Vec<u8>>>, Arc<AtomicBool>);

/// Reads a pipe to its end on a thread, keeping at most `OUTPUT_CAP` bytes.
fn capture<R: Read + Send + 'static>(mut r: R) -> Captured {
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

fn collect(parts: &[Captured], wait: bool) -> String {
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
        Self {
            binary,
            data_root,
            install_timeout: INSTALL_TIMEOUT,
            installing: Arc::new(Mutex::new(HashSet::new())),
            stopping: AtomicBool::new(false),
        }
    }

    /// Stops allowing new extension commands and cancels active ones. Call before waiting for
    /// HTTP requests to finish shutting down.
    pub fn shutdown(&self) {
        let _spawn = self.installing.lock().unwrap_or_else(|e| e.into_inner());
        self.stopping.store(true, Ordering::SeqCst);
    }

    fn spawn(&self, command: &mut Command) -> std::io::Result<ManagedChild> {
        // Serialize the stop flag and the spawn. An initial check before command construction
        // would let a new child start after shutdown had already begun.
        let _spawn = self.installing.lock().unwrap_or_else(|e| e.into_inner());
        if self.stopping.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("OSE is shutting down"));
        }
        ManagedChild::spawn(command)
    }

    /// Overrides the install timeout (tests).
    pub fn with_install_timeout(mut self, timeout: Duration) -> Self {
        self.install_timeout = timeout;
        self
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
        Ok(long_path(&dir))
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
    /// child runs with a cleared environment (allowlist only). Timeout and graceful shutdown
    /// kill the process tree. Blocking: call from `spawn_blocking`.
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
        self.run_extension_command(computer_id, FLAG_INSTALL, &spec, "install")
    }

    /// Runs `<binary> --uninstall-extension <id>` against the computer's data dir, with the rules
    /// of [`OseManager::install_extension`] (SPEC FR-L31.14): the same id pattern (no version), no
    /// shell, one extension command per computer at a time, the timeout and the cleared
    /// environment. Blocking: call from `spawn_blocking`.
    pub fn uninstall_extension(&self, computer_id: &str, id: &str) -> Result<String, InstallError> {
        if !valid_computer_id(computer_id) {
            return Err(InstallError::Bad(format!("invalid computer id: {computer_id:?}")));
        }
        let spec = extension_spec(id, None).map_err(InstallError::Bad)?;
        self.run_extension_command(computer_id, FLAG_UNINSTALL, &spec, "uninstall")
    }

    /// One extension management run of the OSE binary: `<binary> <flag> <spec> <dir args>`. The
    /// caller has validated `computer_id` and `spec`; `what` names the run in error messages.
    fn run_extension_command(
        &self,
        computer_id: &str,
        flag: &str,
        spec: &str,
        what: &str,
    ) -> Result<String, InstallError> {
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
            .arg(flag)
            .arg(spec)
            .args(Self::dir_args(&dir))
            .arg("--accept-server-license-terms")
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = self.spawn(&mut command).map_err(|e| InstallError::Io(format!("cannot start OSE server: {e}")))?;
        let parts = [
            capture(child.child.stdout.take().expect("piped")),
            capture(child.child.stderr.take().expect("piped")),
        ];
        let deadline = Instant::now() + self.install_timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    drop(child);
                    let output = collect(&parts, true);
                    return if status.success() {
                        Ok(output)
                    } else {
                        Err(InstallError::Failed { message: format!("{what} failed: {status}"), output })
                    };
                }
                Ok(None) => {
                    let message = if self.stopping.load(Ordering::SeqCst) {
                        Some(format!("{what} stopped because OSE is shutting down"))
                    } else if Instant::now() >= deadline {
                        Some(format!("{what} timed out after {}s", self.install_timeout.as_secs_f32()))
                    } else {
                        None
                    };
                    if let Some(message) = message {
                        drop(child);
                        return Err(InstallError::Failed { message, output: collect(&parts, false) });
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(InstallError::Io(format!("cannot wait for the OSE server: {e}"))),
            }
        }
    }
}

fn server_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("EMBER_OSE_SERVER").filter(|v| !v.is_empty()) {
        return Some(path.into());
    }
    // Windows (#83): unpacked builds live under %LOCALAPPDATA%, as ~/.local/share elsewhere.
    let share = if cfg!(windows) {
        PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
    } else {
        PathBuf::from(std::env::var_os("HOME")?).join(".local/share")
    };
    let mut candidates: Vec<_> = std::fs::read_dir(share).ok()?.flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("dpx-ose-"))
        .map(|e| launcher_in(&e.path(), cfg!(windows))).filter(|p| p.is_file()).collect();
    candidates.sort();
    candidates.pop()
}

#[cfg(test)]
mod ose_commit_tests {
    use super::ose_commit;

    /// SPEC FR-E10 (#32): the commit comes from the build's `product.json`, beside `bin/`.
    #[test]
    fn fr_e10_ose_commit_is_read_from_the_build_product_json() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("dpx-ose-1.139.1-linux-x64").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let launcher = bin.join("dpx-ose-server");
        std::fs::write(&launcher, "").unwrap();
        assert_eq!(ose_commit(&launcher), None, "no product.json");
        let product = bin.parent().unwrap().join("product.json");
        std::fs::write(&product, r#"{"commit":"  0123456789ABCDEF0123456789abcdef01234567 ","nameShort":"x"}"#).unwrap();
        assert_eq!(ose_commit(&launcher).as_deref(), Some("0123456789abcdef0123456789abcdef01234567"));
        for bad in [r#"{"nameShort":"x"}"#, r#"{"commit":""}"#, r#"{"commit":"not-hex"}"#, r#"{"commit":7}"#, "not json"] {
            std::fs::write(&product, bad).unwrap();
            assert_eq!(ose_commit(&launcher), None, "{bad}");
        }
        std::fs::write(&product, format!(r#"{{"commit":"{}"}}"#, "a".repeat(65))).unwrap();
        assert_eq!(ose_commit(&launcher), None, "longer than 64");
    }
}

#[cfg(test)]
mod tests {
    /// SPEC FR-W4 acceptance step 6 (#83): a missing OSE says what to do, in the words the
    /// release archive uses (`install-ose.sh`, the folder it is in) and where the binary is looked up.
    #[test]
    fn fr_w4_not_installed_names_the_install_step() {
        let m = super::NOT_INSTALLED;
        assert!(m.starts_with("OSE server binary is not installed"), "{m}");
        for word in ["install-ose.sh", "ember-server", "~/.local/share", "EMBER_OSE_SERVER"] {
            assert!(m.contains(word), "{word} missing from {m}");
        }
        assert!(!m.contains("  "), "line continuation left a double space: {m}");
        let script = include_str!("../../server/scripts/install-ose.sh");
        assert!(script.contains("/.local/share"), "install-ose.sh no longer unpacks into ~/.local/share");
    }

    #[test]
    fn fr_w4_windows_strip_verbatim_prefix() {
        use super::strip_verbatim_prefix as s;
        assert_eq!(s(r"\\?\C:\Users\Long Name\x"), r"C:\Users\Long Name\x");
        assert_eq!(s(r"\\?\UNC\server\share\x"), r"\\server\share\x");
        assert_eq!(s(r"C:\Users\x"), r"C:\Users\x");
        assert_eq!(s(r"\\server\share\x"), r"\\server\share\x");
        assert_eq!(s(r"\\?\Volume{abc}\x"), r"\\?\Volume{abc}\x");
        assert_eq!(s("/home/u/x"), "/home/u/x");
    }

    #[cfg(not(windows))]
    #[test]
    fn fr_w4_windows_long_path_is_the_identity_elsewhere() {
        let p = std::path::Path::new("/no/such/dir/../x");
        assert_eq!(super::long_path(p), p.to_path_buf());
    }

    #[cfg(windows)]
    #[test]
    fn fr_w4_windows_long_path_expands_a_short_name() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        #[link(name = "kernel32")]
        extern "system" {
            fn GetShortPathNameW(long: *const u16, short: *mut u16, len: u32) -> u32;
        }
        let tmp = tempfile::tempdir().unwrap();
        let tmp_long = super::long_path(tmp.path());
        let dir = tmp_long.join("Long Directory Name");
        std::fs::create_dir(&dir).unwrap();
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut buf = vec![0u16; 1024];
        // SAFETY: `wide` is NUL-terminated and `buf` holds `buf.len()` u16s.
        let n = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
        assert!(n > 0 && n < buf.len(), "GetShortPathNameW failed");
        let short = std::path::PathBuf::from(std::ffi::OsString::from_wide(&buf[..n]));
        if short == dir {
            println!("8.3 names are disabled on this volume; skipping");
            return;
        }
        assert_eq!(super::long_path(&short), dir);
    }

    use super::*;

    #[test]
    fn fr_l31_24_only_a_network_share_makes_the_ose_poll() {
        use crate::fs_kind::FsKind;
        assert!(watcher_args(FsKind::Local).is_empty());
        assert_eq!(watcher_args(FsKind::Network), ["--file-watcher-polling", "5000"]);
    }

    fn fake_env(k: &str) -> Option<std::ffi::OsString> {
        match k {
            "HOME" => Some("/home/u".into()),
            "HTTPS_PROXY" => Some("http://proxy:3128".into()),
            "https_proxy" => Some("http://lower:3128".into()),
            "NO_PROXY" => Some("localhost".into()),
            "SECRET_TOKEN" => Some("s3cret".into()),
            _ => None,
        }
    }

    fn envs(cmd: &Command) -> Vec<(String, Option<String>)> {
        cmd.get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned())))
            .collect()
    }

    /// FR-P5 (#26): ember-term's folder goes first on the OSE child's PATH (once), and only the
    /// local computer's child learns the built-in node's state dir. No token is set.
    #[test]
    #[cfg(unix)]
    fn fr_p5_ose_child_gets_ember_term_on_path_and_the_local_node_state_dir() {
        let te = TerminalEnv {
            ember_term_dir: Some(PathBuf::from("/opt/ember/bin")),
            local_node_state_dir: Some(PathBuf::from("/home/u/.ember/local-node")),
        };
        let base = std::ffi::OsString::from("/usr/bin:/opt/ember/bin:/bin");
        let v = te.vars(LOCAL_COMPUTER, Some(&base));
        assert_eq!(v, vec![
            ("PATH".to_string(), "/opt/ember/bin:/usr/bin:/bin".into()),
            ("EMBER_NODE_STATE_DIR".to_string(), "/home/u/.ember/local-node".into()),
        ]);
        assert!(!v.iter().any(|(k, _)| k.contains("TOKEN")), "{v:?}");
        let v = te.vars("pi", None);
        assert_eq!(v, vec![("PATH".to_string(), "/opt/ember/bin".into())]);
        assert!(TerminalEnv::default().vars(LOCAL_COMPUTER, Some(&base)).is_empty());
    }

    /// FR-P5 (#26): the command line of an OSE child carries the terminal variables on top of
    /// the allowlist.
    #[test]
    #[cfg(unix)]
    fn fr_p5_ose_command_env_has_ember_term_path() {
        let lookup = |k: &str| match k {
            "PATH" => Some(std::ffi::OsString::from("/usr/bin")),
            "HOME" => Some("/home/u".into()),
            _ => None,
        };
        let mut cmd = ose_command_with(Path::new("/bin/true"), None, lookup);
        let base = cmd.get_envs().find(|(k, _)| k.to_str() == Some("PATH")).and_then(|(_, v)| v.map(|v| v.to_os_string()));
        let te = TerminalEnv { ember_term_dir: Some("/x/bin".into()), local_node_state_dir: Some("/s".into()) };
        for (k, v) in te.vars(LOCAL_COMPUTER, base.as_deref()) {
            cmd.env(k, v);
        }
        let e = envs(&cmd);
        assert!(e.contains(&("PATH".into(), Some("/x/bin:/usr/bin".into()))), "{e:?}");
        assert!(e.contains(&("EMBER_NODE_STATE_DIR".into(), Some("/s".into()))), "{e:?}");
    }

    #[test]
    fn fr_p5_find_ember_term_dir() {
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("ember-term");
        std::fs::write(&bin, b"").unwrap();
        assert_eq!(find_ember_term_dir(Some(bin.clone().into()), None), Some(d.path().to_path_buf()));
        assert_eq!(find_ember_term_dir(Some(d.path().join("nope").into()), None), None);
        assert_eq!(find_ember_term_dir(None, Some(d.path().join("ember-server"))), Some(d.path().to_path_buf()));
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(find_ember_term_dir(None, Some(empty.path().join("ember-server"))), None);
    }

    /// FR-W4 (#471): a listed variable reaches the child, an unlisted one does not.
    #[test]
    fn fr_w4_ose_env_passthrough_listed_arrives_unlisted_does_not() {
        let cmd = ose_command_with(Path::new("/bin/true"), Some("HTTPS_PROXY, https_proxy,NO_PROXY"), fake_env);
        let e = envs(&cmd);
        assert!(e.contains(&("HTTPS_PROXY".into(), Some("http://proxy:3128".into()))), "{e:?}");
        assert!(e.contains(&("https_proxy".into(), Some("http://lower:3128".into()))), "{e:?}");
        assert!(e.contains(&("NO_PROXY".into(), Some("localhost".into()))), "{e:?}");
        assert!(e.contains(&("HOME".into(), Some("/home/u".into()))), "{e:?}");
        assert!(!e.iter().any(|(k, _)| k == "SECRET_TOKEN"), "{e:?}");
        // Without the setting only the allowlist passes.
        let e = envs(&ose_command_with(Path::new("/bin/true"), None, fake_env));
        assert!(!e.iter().any(|(k, _)| k == "HTTPS_PROXY"), "{e:?}");
        let e = envs(&ose_command_with(Path::new("/bin/true"), Some(""), fake_env));
        assert_eq!(e, vec![("HOME".to_string(), Some("/home/u".to_string()))]);
    }

    /// FR-W4 (#471): names are validated; an invalid one is ignored, never passed.
    #[test]
    fn fr_w4_ose_env_passthrough_names_are_validated() {
        for ok in ["HTTP_PROXY", "SSL_CERT_FILE", "_X", "XDG_RUNTIME_DIR", "http_proxy", "no_proxy", "all_proxy"] {
            assert!(valid_passthrough_name(ok), "{ok}");
        }
        for bad in ["", "1ABC", "A=B", "HTTPS_PROXY=x", "lower", "Mixed", "A-B", "A B", "ÄBC", "secret_token"] {
            assert!(!valid_passthrough_name(bad), "{bad}");
        }
        let (ok, bad) = parse_passthrough("HTTPS_PROXY,,A=B, SECRET_token ,HTTPS_PROXY");
        assert_eq!(ok, vec!["HTTPS_PROXY".to_string()]);
        assert_eq!(bad, vec!["A=B".to_string(), "SECRET_token".to_string()]);
        // `A=B` must not smuggle a value through.
        let e = envs(&ose_command_with(Path::new("/bin/true"), Some("HTTPS_PROXY=evil,SECRET_TOKEN=x"), fake_env));
        assert!(!e.iter().any(|(k, _)| k.contains('=') || k == "SECRET_TOKEN"), "{e:?}");
    }

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

    /// FR-W4 Windows (#83, D49): the launcher in an unpacked build is `bin/dpx-ose-server` on
    /// Linux and macOS and `bin/dpx-ose-server.cmd` on Windows.
    #[test]
    fn fr_w4_win_launcher_name_per_os() {
        assert_eq!(launcher_name(false), "dpx-ose-server");
        assert_eq!(launcher_name(true), "dpx-ose-server.cmd");
        let root = Path::new("/opt/dpx-ose-1.0.0-linux-x64");
        assert_eq!(launcher_in(root, false), root.join("bin").join("dpx-ose-server"));
        assert_eq!(launcher_in(root, true), root.join("bin").join("dpx-ose-server.cmd"));
    }

    /// FR-W4 Windows (#83, D49): a `.cmd` launcher is not spawned through `cmd.exe`; the program
    /// is the bundled `node.exe` and the first argument `out/server-main.js`, which is what the
    /// upstream `.cmd` runs. The shell launcher runs as is.
    #[test]
    fn fr_w4_win_cmd_launcher_runs_the_bundled_node() {
        let root = PathBuf::from("C:/ose/dpx-ose-1.0.0-windows-x64");
        for name in ["dpx-ose-server.cmd", "DPX-OSE-SERVER.CMD", "dpx-ose-server.bat"] {
            let (program, args) = launch_program(&root.join("bin").join(name));
            assert_eq!(program, root.join("node.exe"), "{name}");
            assert_eq!(args, vec![root.join("out").join("server-main.js").into_os_string()], "{name}");
        }
        let unix = Path::new("/opt/ose/bin/dpx-ose-server");
        let (program, args) = launch_program(unix);
        assert_eq!(program, unix);
        assert!(args.is_empty());
    }

    /// FR-W4 Windows (#83, D49): the child's command line starts with the server script, then the
    /// caller's flags.
    #[test]
    fn fr_w4_win_command_puts_the_server_script_first() {
        let root = PathBuf::from("C:/ose/x");
        let cmd = ose_command_with(&root.join("bin/dpx-ose-server.cmd"), None, fake_env);
        assert_eq!(Path::new(cmd.get_program()), root.join("node.exe"));
        let args: Vec<_> = cmd.get_args().map(|a| a.to_os_string()).collect();
        assert_eq!(args, vec![root.join("out").join("server-main.js").into_os_string()]);
    }

    /// FR-W4 Windows (#83, D49): the cleared environment keeps what Node needs on Windows
    /// (without `SystemRoot` sockets and crypto fail); the Unix list is unchanged.
    #[test]
    fn fr_w4_win_env_allowlist_adds_the_windows_names() {
        let unix = env_allowlist(false);
        assert_eq!(unix, ENV_ALLOWLIST.to_vec());
        let win = env_allowlist(true);
        for name in ["SystemRoot", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP", "PATHEXT", "COMSPEC"] {
            assert!(win.contains(&name), "{name}");
        }
        for name in ENV_ALLOWLIST {
            assert!(win.contains(&name), "{name}");
        }
    }
}
