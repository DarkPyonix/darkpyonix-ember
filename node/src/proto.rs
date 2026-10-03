//! Wire types shared by the daemon ([`crate::api`]) and the client ([`crate::client`]).
//!
//! Every request and response is JSON. Byte payloads (file contents, process output, stdin) are
//! base64 strings so that one encoding carries over any transport — the node ↔ server transport
//! is not decided yet (`INTENT.md` Q7). Bump [`PROTOCOL_VERSION`] on any incompatible change.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Incremented on incompatible wire changes; reported by `/v1/health`.
pub const PROTOCOL_VERSION: u32 = 1;

/// Serde helper: `Vec<u8>` as a standard base64 string.
pub mod b64 {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(s).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------------------------
// Errors

/// Body of every non-2xx response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub error: String,
    /// Set on [`ErrorCode::PreconditionFailed`]: the file's current hash, `None` if it is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_sha256: Option<String>,
    /// For a failed file operation: the portable name of the OS error (`ENOENT`, `ENOTEMPTY`,
    /// `EEXIST`, …), so a client on another OS can map it to its own errno (numbers differ
    /// between macOS and Linux). Policy refusals are `EACCES`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errno: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unauthorized,
    /// The path resolves outside every allowed root.
    ForbiddenPath,
    NotFound,
    PreconditionFailed,
    BadRequest,
    Internal,
}

// ---------------------------------------------------------------------------------------------
// Health and environment

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    pub ok: bool,
    pub version: String,
    pub protocol: u32,
}

/// Description of the computer, for the agent's replaceable environment block (FR-X3, FR-S7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvInfo {
    /// `std::env::consts::OS`: `macos`, `linux`, …
    pub os: String,
    /// Human-readable OS release, e.g. `macOS 26.5` or the `PRETTY_NAME` of `/etc/os-release`.
    pub os_version: Option<String>,
    /// Kernel release (`uname -r`).
    pub kernel: Option<String>,
    /// `std::env::consts::ARCH`: `aarch64`, `x86_64`, …
    pub arch: String,
    pub hostname: String,
    pub user: Option<String>,
    pub home: Option<PathBuf>,
    /// The user's login shell (`$SHELL`).
    pub shell: Option<String>,
    /// The roots file and exec operations are confined to.
    pub roots: Vec<PathBuf>,
    /// Toolchains found on the daemon's `PATH`, keyed by tool name.
    pub toolchains: BTreeMap<String, Toolchain>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Toolchain {
    pub path: PathBuf,
    /// First non-empty line of `<tool> --version` (or the tool's equivalent).
    pub version: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Files

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathRequest {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stat {
    /// The canonical path (symlinks resolved; for `lstat`, the parent's canonical path plus the
    /// final component as given).
    pub path: PathBuf,
    pub kind: FileKind,
    pub size: u64,
    /// Modification time, milliseconds since the Unix epoch.
    pub mtime_ms: Option<u64>,
    /// Unix permission bits.
    pub mode: u32,
    pub readonly: bool,
    /// Inode number on the node (absent from older nodes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ino: Option<u64>,
    /// Hard link count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nlink: Option<u64>,
    /// Access time, milliseconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atime_ms: Option<u64>,
    /// Status change time, milliseconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctime_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadRequest {
    pub path: PathBuf,
    /// First byte to return (default 0).
    #[serde(default)]
    pub offset: u64,
    /// Maximum bytes to return (default and cap: [`MAX_READ`]).
    #[serde(default)]
    pub len: Option<u64>,
    /// Hash the whole file (default true). Turn off for large files when only a range is needed.
    #[serde(default = "yes")]
    pub hash: bool,
}

/// Largest range one read returns.
pub const MAX_READ: u64 = 16 * 1024 * 1024;

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadResponse {
    pub path: PathBuf,
    /// Size of the whole file.
    pub size: u64,
    pub mtime_ms: Option<u64>,
    /// SHA-256 of the **whole file** (hex), the observation key of FR-S7. `None` if `hash: false`.
    pub sha256: Option<String>,
    pub offset: u64,
    #[serde(with = "b64")]
    pub data: Vec<u8>,
    /// True when the returned range reaches the end of the file.
    pub eof: bool,
}

/// Write precondition, checked under the daemon's write lock immediately before the rename.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "sha256")]
pub enum Expect {
    /// The file must not exist (create-only).
    Absent,
    /// The file must exist with exactly this SHA-256 (hex).
    Sha256(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteRequest {
    pub path: PathBuf,
    #[serde(with = "b64")]
    pub data: Vec<u8>,
    #[serde(default)]
    pub expect: Option<Expect>,
    /// Create missing parent directories (inside an allowed root).
    #[serde(default)]
    pub create_parents: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteResponse {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub kind: FileKind,
    pub size: u64,
    pub mtime_ms: Option<u64>,
    /// Unix mode bits of the entry itself (not following a symlink); absent from older nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ino: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListResponse {
    pub path: PathBuf,
    /// Sorted by name.
    pub entries: Vec<DirEntry>,
}

/// `POST /v1/fs/readlink` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadlinkResponse {
    pub path: PathBuf,
    /// The link's target exactly as stored (may be relative, may point outside the roots).
    pub target: PathBuf,
}

/// Create a symbolic link at `path` pointing to `target`. The target is stored as given and is
/// not checked against the roots (following it through the file API is).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymlinkRequest {
    pub path: PathBuf,
    pub target: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MkdirRequest {
    pub path: PathBuf,
    /// Create missing parents too, and succeed if the directory already exists (`mkdir -p`).
    #[serde(default)]
    pub parents: bool,
    /// Permission bits for the new directory (default 0o777 minus the daemon's umask).
    #[serde(default)]
    pub mode: Option<u32>,
}

/// Remove a file, symbolic link (never its target) or directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveRequest {
    pub path: PathBuf,
    /// For a directory: remove its contents too (`rm -r`). Without it only an empty directory is
    /// removed (`ENOTEMPTY` otherwise).
    #[serde(default)]
    pub recursive: bool,
}

/// Rename (move) within the roots. Both paths name the entries themselves: a symbolic link is
/// moved, not its target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameRequest {
    pub from: PathBuf,
    pub to: PathBuf,
    /// Replace an existing `to` (default true, as `rename(2)`). When false an existing `to` is
    /// `EEXIST`; the check is not atomic with the rename.
    #[serde(default = "yes")]
    pub overwrite: bool,
}

/// A time to set: the node's current time, or an explicit one.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "ms")]
pub enum SetTime {
    Now,
    /// Milliseconds since the Unix epoch.
    UnixMs(u64),
}

/// Change attributes in one call: what NFS `SETATTR` and FUSE `setattr` need. Fields left out
/// are unchanged. Applied in the order size, mode, times. Follows a symbolic link (like
/// `chmod(2)`, `truncate(2)`, `utimensat(2)` without `AT_SYMLINK_NOFOLLOW`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetAttrRequest {
    pub path: PathBuf,
    /// Permission bits (`chmod`).
    #[serde(default)]
    pub mode: Option<u32>,
    /// New length (`truncate`); extends with zeros. In place, not atomic.
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub atime: Option<SetTime>,
    #[serde(default)]
    pub mtime: Option<SetTime>,
}

/// Write `data` at `offset` into an existing regular file, in place (not atomic, unlike
/// [`WriteRequest`]): the primitive a mounted filesystem's `write` needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PwriteRequest {
    pub path: PathBuf,
    pub offset: u64,
    #[serde(with = "b64")]
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobRequest {
    /// Directory to search under.
    pub root: PathBuf,
    /// Glob matched against the path relative to `root`; `*` does not cross `/`, `**` does.
    pub pattern: String,
    /// Skip files ignored by `.gitignore`/`.ignore` and hidden files (default true, like `rg`).
    #[serde(default = "yes")]
    pub respect_ignore: bool,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobResponse {
    /// Absolute paths, most recently modified first.
    pub paths: Vec<PathBuf>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepRequest {
    /// File or directory to search.
    pub root: PathBuf,
    /// Regular expression (Rust `regex` syntax, as ripgrep).
    pub pattern: String,
    /// Only search files whose path relative to `root` matches this glob.
    #[serde(default)]
    pub glob: Option<String>,
    #[serde(default)]
    pub case_insensitive: bool,
    #[serde(default = "yes")]
    pub respect_ignore: bool,
    /// Maximum matching lines returned (default 1000).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrepMatch {
    pub path: PathBuf,
    /// 1-based.
    pub line: u64,
    /// The matching line without its terminator, lossily decoded, truncated to 2000 bytes.
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepResponse {
    pub matches: Vec<GrepMatch>,
    pub truncated: bool,
}

// ---------------------------------------------------------------------------------------------
// Commands

/// What to run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Program {
    /// Run this argv directly (no shell).
    Argv(Vec<String>),
    /// Run through `/bin/sh -c`. Use `Argv(["zsh", "-lc", …])` for another shell.
    Shell(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSpec {
    pub program: Program,
    /// Working directory; must be inside an allowed root.
    pub cwd: PathBuf,
    /// Variables added to (or overriding) the daemon's environment.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Start from an empty environment instead of the daemon's.
    #[serde(default)]
    pub env_clear: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct PtySize {
    pub rows: u16,
    pub cols: u16,
}

/// First message a client sends on `/v1/exec`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRequest {
    #[serde(flatten)]
    pub command: CommandSpec,
    /// Run under a pseudo-terminal of this size. Output then arrives as `stdout` only.
    #[serde(default)]
    pub pty: Option<PtySize>,
}

/// Client → daemon messages on `/v1/exec` after the [`ExecRequest`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ExecInput {
    Stdin {
        #[serde(with = "b64")]
        data: Vec<u8>,
    },
    /// Close stdin (pipe mode). In PTY mode send `\x04` as stdin instead.
    CloseStdin,
    /// PTY mode only.
    Resize { size: PtySize },
    /// Signal the process group (default `SIGTERM`).
    Kill {
        #[serde(default)]
        signal: Option<i32>,
    },
}

/// Daemon → client messages on `/v1/exec`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ExecEvent {
    Started { pid: u32 },
    Stdout {
        #[serde(with = "b64")]
        data: Vec<u8>,
    },
    Stderr {
        #[serde(with = "b64")]
        data: Vec<u8>,
    },
    /// Last message. Exactly one of `code` and `signal` is set.
    Exit { code: Option<i32>, signal: Option<i32> },
    /// The command could not be started or the session failed; last message.
    Error { message: String },
}

// ---------------------------------------------------------------------------------------------
// Background jobs (FR-X4)

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRequest {
    #[serde(flatten)]
    pub command: CommandSpec,
    /// Free-form label shown in listings (e.g. the session that started it).
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Exited,
    Signaled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobInfo {
    pub id: String,
    pub label: Option<String>,
    pub program: Program,
    pub cwd: PathBuf,
    pub pid: Option<u32>,
    pub state: JobState,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    /// Total bytes of output produced so far (stdout and stderr together).
    pub output_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobDetail {
    #[serde(flatten)]
    pub info: JobInfo,
    /// The last bytes of combined stdout+stderr, lossily decoded.
    pub tail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KillRequest {
    #[serde(default)]
    pub signal: Option<i32>,
}

/// Node → server notifications on `/v1/events?after=<seq>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeEvent {
    /// Strictly increasing for the daemon's lifetime; resume with `after=<last seen>`.
    pub seq: u64,
    /// Identifies this daemon run; a change means sequence numbers restarted.
    pub boot_id: String,
    #[serde(flatten)]
    pub kind: NodeEventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum NodeEventKind {
    JobStarted { job: JobInfo },
    JobFinished {
        job: JobInfo,
        /// Output tail at completion (same as [`JobDetail::tail`]).
        tail: String,
    },
    /// A persistent terminal session was created (or re-adopted after a node restart).
    TermStarted { term: TermInfo },
    /// A persistent terminal session's process exited, was killed, or was found lost.
    TermFinished { term: TermInfo },
}

// ---------------------------------------------------------------------------------------------
// Persistent terminal sessions (SPEC §P, docs/design/TERMINALS.md)

/// What started a persistent terminal session (FR-P6).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum TermOrigin {
    /// A VS Code (IDE window) terminal, task or debuggee.
    IdeVscode,
    /// The Ember editor's terminal panel or run action.
    IdeEmber,
    /// An agent's tool call.
    Agent,
    /// Started by hand (e.g. `ember-term` in a plain terminal).
    User,
}

impl TermOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            TermOrigin::IdeVscode => "ide-vscode",
            TermOrigin::IdeEmber => "ide-ember",
            TermOrigin::Agent => "agent",
            TermOrigin::User => "user",
        }
    }
}

impl std::str::FromStr for TermOrigin {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "ide-vscode" => TermOrigin::IdeVscode,
            "ide-ember" => TermOrigin::IdeEmber,
            "agent" => TermOrigin::Agent,
            "user" => TermOrigin::User,
            other => return Err(format!("unknown origin {other:?} (ide-vscode, ide-ember, agent, user)")),
        })
    }
}

/// `POST /v1/terms`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermCreateRequest {
    /// What to run. `None`: the login shell of the daemon's user (`$SHELL`, else `/bin/sh`;
    /// with `-l` on macOS, as VS Code does).
    #[serde(default)]
    pub program: Option<Program>,
    /// Working directory; must be inside an allowed root.
    pub cwd: PathBuf,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Start from an empty environment plus `env` (a client forwarding its whole environment).
    #[serde(default)]
    pub env_clear: bool,
    /// Initial size (default 24×80). Later the size follows the attached clients (FR-P4).
    #[serde(default)]
    pub size: Option<PtySize>,
    pub origin: TermOrigin,
    /// The project the session belongs to (normally the workspace folder's absolute path);
    /// listings filter on it.
    #[serde(default)]
    pub project: Option<String>,
    /// Display name; the program's OSC title is reported separately.
    #[serde(default)]
    pub title: Option<String>,
    /// Idempotency key: if a **running** session already has this key, it is returned instead
    /// of starting a new one (`created: false`). The attach-or-create primitive.
    #[serde(default)]
    pub key: Option<String>,
    /// Free-form labels for clients (e.g. `vscode.pid`, `vscode.mode`).
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermCreateResponse {
    /// False when `key` matched a running session.
    pub created: bool,
    pub term: TermInfo,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TermState {
    Running,
    /// The process exited (or was killed).
    Exited,
    /// Found running in the metadata of a previous node run but could not be re-adopted: the
    /// process died with that node, or its PTY could not be recovered.
    Lost,
}

/// One attached client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TermClient {
    /// Unique within the session for the daemon's lifetime.
    pub client: u64,
    /// Human-readable device / window label shown in "controlled by <device>".
    pub device: String,
    /// Client kind, e.g. `ember-term`, `ember-editor`, `agent`.
    #[serde(default)]
    pub kind: Option<String>,
    /// The client's process id on this computer, if it runs here (ember-term).
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub read_only: bool,
    /// The client's own viewport size, if it reported one.
    #[serde(default)]
    pub size: Option<PtySize>,
    pub attached_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TermInfo {
    pub id: String,
    #[serde(default)]
    pub key: Option<String>,
    /// The requested title, else the program's last OSC title, else the program name.
    pub title: String,
    pub origin: TermOrigin,
    #[serde(default)]
    pub project: Option<String>,
    /// The argv actually started.
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    /// The session leader (the shell); `None` once finished.
    pub pid: Option<u32>,
    pub state: TermState,
    /// Exit status. Both `None` after `Exited` means unknown (the process was re-adopted after
    /// a node restart and is not the daemon's child, so its status cannot be collected).
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub size: PtySize,
    pub created_ms: u64,
    pub finished_ms: Option<u64>,
    /// Last output or input.
    pub last_activity_ms: u64,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
    pub clients: Vec<TermClient>,
    /// The client that has taken control, if any (FR-P4).
    pub controller: Option<TermClient>,
    /// A PTY keeper holds the session's PTY, so the process survives an ember node restart
    /// (best effort).
    pub survives_node_restart: bool,
    /// This session was re-adopted from a previous node run.
    pub adopted: bool,
    /// Attaching yields a screen snapshot (false for lost sessions and for finished sessions
    /// loaded from disk).
    pub has_screen: bool,
}

/// `GET /v1/terms` query.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TermListQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<TermOrigin>,
    /// Only running (`true`) or only finished (`false`) sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running: Option<bool>,
}

/// `GET /v1/terms/{id}/snapshot`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermSnapshot {
    pub size: PtySize,
    /// Escape sequences that redraw scrollback, screen, cursor and modes on a reset terminal.
    #[serde(with = "b64")]
    pub data: Vec<u8>,
    /// The visible screen as plain text, one line per row (trailing blanks trimmed).
    pub text: String,
}

/// `POST /v1/terms/{id}/control`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermControlRequest {
    /// The attached client that takes or releases control.
    pub client: u64,
    pub take: bool,
}

/// First message a client sends on `/v1/terms/{id}/attach`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermHello {
    pub device: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    /// The client's viewport size.
    #[serde(default)]
    pub size: Option<PtySize>,
    /// Count this attach as activity, so the session takes this client's size when nobody has
    /// control. A background re-attach (e.g. restoring the terminal list) sets `false`.
    #[serde(default)]
    pub active: bool,
    /// Never send input (viewer).
    #[serde(default)]
    pub read_only: bool,
    /// Send a [`TermEvent::Snapshot`] first (default true).
    #[serde(default = "yes")]
    pub snapshot: bool,
}

/// Client → daemon messages on `/v1/terms/{id}/attach` after the [`TermHello`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum TermInput {
    /// Keystrokes / paste. Serialized with other clients' input in arrival order.
    Input {
        #[serde(with = "b64")]
        data: Vec<u8>,
    },
    /// This client's viewport size.
    Resize { size: PtySize },
    TakeControl,
    ReleaseControl,
    /// Signal the session (default SIGHUP, escalating to SIGKILL after a grace period).
    Kill {
        #[serde(default)]
        signal: Option<i32>,
    },
    /// Detach (same as closing the socket). Never ends the session.
    Detach,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TermRefusal {
    /// Another client has control.
    Controlled,
    /// This client attached read-only.
    ReadOnly,
    /// The session is not running.
    NotRunning,
}

/// Daemon → client messages on `/v1/terms/{id}/attach`.
// A wire message: `Attached` is sent once per attach, so its size does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum TermEvent {
    /// First message: this client's id and the session.
    Attached { client: u64, term: TermInfo },
    /// Reset the terminal, then write `data` (scrollback, screen, cursor, modes, title).
    /// Sent once, right after `attached`; `output` continues exactly where it ends.
    Snapshot {
        size: PtySize,
        #[serde(with = "b64")]
        data: Vec<u8>,
    },
    /// Raw PTY output.
    Output {
        #[serde(with = "b64")]
        data: Vec<u8>,
    },
    /// The PTY size changed.
    Resized { size: PtySize },
    /// Control changed hands (`None`: released; everyone may type).
    Control { controller: Option<TermClient> },
    /// This client's input was not delivered.
    Refused { reason: TermRefusal, controller: Option<TermClient> },
    /// The set of attached clients changed.
    Clients { clients: Vec<TermClient> },
    /// The program set (or reset) its title.
    Title { title: Option<String> },
    /// The process exited; last message. Both `None`: status unknown (re-adopted session).
    Exit { code: Option<i32>, signal: Option<i32> },
    /// Attach failed, or this client was dropped (e.g. too slow); last message.
    Error { message: String },
}
