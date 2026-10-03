//! Project mount: Claude Code's file tools on the session's current computer (#6, FR-X2).
//!
//! Claude Code's Read, Edit, Write, Glob and Grep run inside the `claude` process on **this
//! server** (Grep through its embedded ripgrep). For a session on another computer, the node's
//! project directory is mounted here **at the same absolute path**, so the paths the agent sees
//! are the node's and every file syscall goes to the node's file API (`docs/design/
//! INTERCEPTION.md`, option (a); `docs/design/COMPUTERS.md` § Project mount). Bash already runs
//! on the node through `ember-exec` ([`super::shim`]).
//!
//! | Server OS | Mechanism (feature) | Privileges |
//! |-|-|-|
//! | macOS (Mac mini) | loopback NFSv3 server in this process ([`nfs`], `nfsserve`) + `/sbin/mount_nfs` (`mount-nfs`) | none for a mount point this user owns **[U]**; creating a missing mount point outside the user's directories needs a one-time `sudo` (and `/etc/auto_master` for `/home`, `/etc/synthetic.conf` for a new top-level directory) |
//! | Linux (Raspberry Pi) | FUSE in this process ([`fuse`], `fuser`) via setuid `fusermount3` (`mount-fuse`) | none for an owned mount point (package `fuse3`); creating `/Users/<u>/…` or `/home/<other>/…` needs a one-time `sudo mkdir`+`chown` |
//!
//! Without either feature, or with `EMBER_MOUNT=off`, nothing is mounted and the file tools stay
//! local (the behaviour before this module).
//!
//! # Lifecycle
//!
//! - **Mount** before a Claude Code session's agent starts on a node (a [`crate::session::Sessions`] prepare hook,
//!   `Computers::install`): the session's cwd is mounted, or an existing mount covering it (same
//!   computer) is shared. Failure fails the start with a clear message.
//! - **Unmount** when no session needs it: on a switch away ([`ProjectMounts::release`]), when a
//!   periodic sweep finds none of its sessions live (idle release, exit, deletion), and at
//!   shutdown. The mount point directories this module created are removed again.
//! - **Freshness**: the `ember-exec` shim sends `invalidate` on a private datagram socket
//!   ([`CtlSocket`], `EMBER_MOUNT_CTL`) after every remote command, so what Bash changed on the
//!   node is seen by the next Read; other changes on the node show within the attribute TTL (1 s).
//!
//! # Failure behaviour
//!
//! Every node call has a deadline ([`remote_fs::CacheConfig::op_timeout`], 4 s) shorter than the
//! NFS client's per-try timeout (5 s), and NFS mounts are `soft` with `deadtimeout`, so a lost
//! node turns into `EIO`/`ETIMEDOUT` from the syscall — never an unkillable process. The outage is
//! also put in front of the agent's next message as a system notice
//! ([`ProjectMounts::outage_notice`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use ember_node::client::NodeClient;

use super::Computer;

pub mod cmd;
#[cfg(all(feature = "mount-fuse", target_os = "linux"))]
pub mod fuse;
#[cfg(feature = "mount-nfs")]
pub mod nfs;
pub mod remote_fs;

use cmd::{MountPoint, Os};
use remote_fs::{CacheConfig, FsErr, NodeFs, RemoteFs, ROOT_ID};

/// `off`, `nfs`, `fuse` or `auto` (default): which mechanism mounts project directories.
pub const ENV_MOUNT: &str = "EMBER_MOUNT";
/// `1` allows mounting over a non-empty directory of the same path on this server.
pub const ENV_SHADOW: &str = "EMBER_MOUNT_SHADOW";
/// Attribute / listing cache lifetime in milliseconds (default 1000).
pub const ENV_TTL_MS: &str = "EMBER_MOUNT_TTL_MS";
/// Path of the control socket, given to the `ember-exec` shim.
pub const ENV_CTL: &str = "EMBER_MOUNT_CTL";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    Off,
    Nfs,
    Fuse,
}

#[derive(Debug, Clone)]
pub struct MountSettings {
    pub mechanism: Mechanism,
    pub cache: CacheConfig,
    /// Mount over a non-empty directory (hiding it while mounted).
    pub allow_shadow: bool,
    /// How long a mount with no live session is kept before unmounting.
    pub idle_grace: Duration,
}

impl MountSettings {
    /// What this build and platform support, overridden by [`ENV_MOUNT`].
    pub fn from_env() -> MountSettings {
        Self::from_vars(|k| std::env::var(k).ok())
    }

    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> MountSettings {
        let auto = if cfg!(all(feature = "mount-fuse", target_os = "linux")) {
            Mechanism::Fuse
        } else if cfg!(feature = "mount-nfs") {
            Mechanism::Nfs
        } else {
            Mechanism::Off
        };
        let mechanism = match get(ENV_MOUNT).as_deref() {
            Some("off" | "0" | "false") => Mechanism::Off,
            Some("nfs") => Mechanism::Nfs,
            Some("fuse") => Mechanism::Fuse,
            _ => auto,
        };
        let mut cache = CacheConfig { op_timeout: Duration::from_secs(4), ..CacheConfig::default() };
        if let Some(ms) = get(ENV_TTL_MS).and_then(|v| v.parse().ok()) {
            cache.attr_ttl = Duration::from_millis(ms);
        }
        MountSettings {
            mechanism,
            cache,
            allow_shadow: get(ENV_SHADOW).as_deref() == Some("1"),
            idle_grace: Duration::from_secs(120),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Mechanisms

/// A live kernel mount.
#[async_trait]
pub trait ActiveMount: Send + Sync {
    async fn unmount(self: Box<Self>) -> anyhow::Result<()>;
}

/// Puts a [`RemoteFs`] into the kernel at a path.
#[async_trait]
pub trait Mounter: Send + Sync {
    fn name(&self) -> &'static str;
    /// Mount `fs` at `at`, an existing directory owned by this user.
    async fn mount(&self, fs: Arc<RemoteFs>, at: &Path) -> anyhow::Result<Box<dyn ActiveMount>>;
    /// Remove whatever is mounted at `at` (a stale mount left by an earlier run).
    async fn force_unmount(&self, at: &Path) -> anyhow::Result<()>;
}

/// No mechanism: every mount attempt fails with an explanation.
pub struct NoMount;

#[async_trait]
impl Mounter for NoMount {
    fn name(&self) -> &'static str {
        "none"
    }

    async fn mount(&self, _fs: Arc<RemoteFs>, at: &Path) -> anyhow::Result<Box<dyn ActiveMount>> {
        anyhow::bail!(
            "cannot mount {}: this ember server was built without a project mount (features \
             mount-nfs on macOS, mount-fuse on Linux)",
            at.display()
        )
    }

    async fn force_unmount(&self, _at: &Path) -> anyhow::Result<()> {
        Ok(())
    }
}

/// How long a mount or unmount command may run.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);

/// Run a mount/unmount command with a deadline; the error carries its stderr.
pub async fn run_cmd(c: &cmd::Cmd) -> anyhow::Result<()> {
    let out = tokio::time::timeout(COMMAND_TIMEOUT, c.to_tokio().output())
        .await
        .map_err(|_| anyhow::anyhow!("{} timed out", c.program.display()))??;
    if out.status.success() {
        Ok(())
    } else {
        anyhow::bail!(
            "{} {} failed ({}): {}",
            c.program.display(),
            c.args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )
    }
}

/// Unmount gently, then by force. Only the NFS and FUSE mounters call it.
#[cfg_attr(not(any(feature = "mount-nfs", feature = "mount-fuse")), allow(dead_code))]
async fn unmount_at(os: Os, fuse: bool, at: &Path) -> anyhow::Result<()> {
    match run_cmd(&cmd::unmount(os, fuse, at, false)).await {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::warn!(path = %at.display(), "unmount failed ({e:#}); forcing");
            run_cmd(&cmd::unmount(os, fuse, at, true)).await
        }
    }
}

/// Loopback NFS server + the OS NFS client.
#[cfg(feature = "mount-nfs")]
pub struct NfsMounter {
    pub os: Os,
}

#[cfg(feature = "mount-nfs")]
struct NfsActive {
    os: Os,
    at: PathBuf,
    server: nfs::NfsServer,
}

#[cfg(feature = "mount-nfs")]
#[async_trait]
impl ActiveMount for NfsActive {
    async fn unmount(self: Box<Self>) -> anyhow::Result<()> {
        let res = unmount_at(self.os, false, &self.at).await;
        // Stop serving only after the kernel let go (or gave up).
        self.server.stop();
        res
    }
}

#[cfg(feature = "mount-nfs")]
#[async_trait]
impl Mounter for NfsMounter {
    fn name(&self) -> &'static str {
        "nfs"
    }

    async fn mount(&self, fs: Arc<RemoteFs>, at: &Path) -> anyhow::Result<Box<dyn ActiveMount>> {
        let server = nfs::NfsServer::start(fs).await?;
        let c = cmd::nfs_mount(self.os, &cmd::NfsOptions::new(server.port), at);
        if let Err(e) = run_cmd(&c).await {
            server.stop();
            return Err(e);
        }
        Ok(Box::new(NfsActive { os: self.os, at: at.to_path_buf(), server }))
    }

    async fn force_unmount(&self, at: &Path) -> anyhow::Result<()> {
        run_cmd(&cmd::unmount(self.os, false, at, true)).await
    }
}

/// In-process FUSE (Linux).
#[cfg(all(feature = "mount-fuse", target_os = "linux"))]
pub struct FuseMounter {
    pub ttl: Duration,
}

#[cfg(all(feature = "mount-fuse", target_os = "linux"))]
struct FuseActive(fuse::FuseMount);

#[cfg(all(feature = "mount-fuse", target_os = "linux"))]
#[async_trait]
impl ActiveMount for FuseActive {
    async fn unmount(self: Box<Self>) -> anyhow::Result<()> {
        let m = self.0;
        tokio::task::spawn_blocking(move || m.unmount()).await??;
        Ok(())
    }
}

#[cfg(all(feature = "mount-fuse", target_os = "linux"))]
#[async_trait]
impl Mounter for FuseMounter {
    fn name(&self) -> &'static str {
        "fuse"
    }

    async fn mount(&self, fs: Arc<RemoteFs>, at: &Path) -> anyhow::Result<Box<dyn ActiveMount>> {
        let (rt, ttl, at) = (tokio::runtime::Handle::current(), self.ttl, at.to_path_buf());
        let m = tokio::task::spawn_blocking(move || fuse::FuseMount::mount(fs, &at, ttl, rt)).await??;
        Ok(Box::new(FuseActive(m)))
    }

    async fn force_unmount(&self, at: &Path) -> anyhow::Result<()> {
        run_cmd(&cmd::unmount(Os::Linux, true, at, true)).await
    }
}

/// The mounter for `settings` on this platform and build.
pub fn mounter_for(settings: &MountSettings) -> Arc<dyn Mounter> {
    match settings.mechanism {
        #[cfg(feature = "mount-nfs")]
        Mechanism::Nfs => match Os::current() {
            Some(os) => Arc::new(NfsMounter { os }),
            None => Arc::new(NoMount),
        },
        #[cfg(all(feature = "mount-fuse", target_os = "linux"))]
        Mechanism::Fuse => Arc::new(FuseMounter { ttl: settings.cache.attr_ttl }),
        _ => Arc::new(NoMount),
    }
}

// ---------------------------------------------------------------------------------------------
// Orchestration

/// Opens the node file API for a computer.
pub type NodeFsConnector = Arc<dyn Fn(&Computer, Duration) -> anyhow::Result<Arc<dyn NodeFs>> + Send + Sync>;

/// [`NodeClient`] with a request timeout.
pub fn default_node_fs() -> NodeFsConnector {
    Arc::new(|c: &Computer, timeout: Duration| -> anyhow::Result<Arc<dyn NodeFs>> {
        Ok(Arc::new(NodeClient::with_timeout(&c.url, &c.token, timeout)?))
    })
}

#[derive(Debug, thiserror::Error)]
pub enum MountError {
    #[error("project mount: {0} is not an absolute path")]
    NotAbsolute(PathBuf),
    #[error("project mount: {path} is already mounted from another computer ({other}); sessions on different computers cannot share a path")]
    Conflict { path: PathBuf, other: String },
    #[error("project mount: {path} would contain or be inside the mount of {other}; nested project directories are not supported")]
    Nested { path: PathBuf, other: PathBuf },
    #[error("project mount: computer \"{computer}\" is unreachable: {error}")]
    Unreachable { computer: String, error: String },
    #[error("project mount: {path} does not exist on computer \"{computer}\"")]
    MissingOnNode { path: PathBuf, computer: String },
    #[error("project mount: {path} on computer \"{computer}\" is outside the directories its ember node allows (its roots)")]
    Forbidden { path: PathBuf, computer: String },
    #[error("project mount: {path} on computer \"{computer}\" is not a directory")]
    NotADirectory { path: PathBuf, computer: String },
    #[error("project mount: {0}")]
    MountPoint(#[from] cmd::MountPointError),
    #[error("project mount: mounting {path} failed: {error:#}")]
    Mount { path: PathBuf, error: anyhow::Error },
}

struct Entry {
    computer_id: String,
    computer_name: String,
    fs: Arc<RemoteFs>,
    active: Option<Box<dyn ActiveMount>>,
    users: HashSet<String>,
    /// Mount point directories this module created (outermost first), removed on unmount.
    created: Vec<PathBuf>,
    /// Since when no user's agent was live (the sweep unmounts after [`MountSettings::idle_grace`]).
    unused_since: Option<SystemTime>,
}

/// A mount, for listing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MountInfo {
    pub path: PathBuf,
    pub computer_id: String,
    pub sessions: Vec<String>,
    pub outage: Option<String>,
}

/// All project mounts of this server.
pub struct ProjectMounts {
    mounter: Arc<dyn Mounter>,
    connect: NodeFsConnector,
    settings: MountSettings,
    os: Os,
    /// Held across mount/unmount so two starts never race on one path.
    entries: tokio::sync::Mutex<HashMap<PathBuf, Entry>>,
    /// Sync view for [`ProjectMounts::invalidate_all`] and [`ProjectMounts::covers`].
    index: Mutex<Vec<(PathBuf, Arc<RemoteFs>)>>,
    /// Per session: the outage start already reported to it.
    notified: Mutex<HashMap<String, SystemTime>>,
}

impl ProjectMounts {
    pub fn new(mounter: Arc<dyn Mounter>, connect: NodeFsConnector, settings: MountSettings) -> Arc<ProjectMounts> {
        Arc::new(ProjectMounts {
            mounter,
            connect,
            settings,
            os: Os::current().unwrap_or(Os::Linux),
            entries: tokio::sync::Mutex::new(HashMap::new()),
            index: Mutex::new(Vec::new()),
            notified: Mutex::new(HashMap::new()),
        })
    }

    /// From the environment, or `None` when mounting is off.
    pub fn from_env() -> Option<Arc<ProjectMounts>> {
        let settings = MountSettings::from_env();
        if settings.mechanism == Mechanism::Off {
            return None;
        }
        let mounter = mounter_for(&settings);
        tracing::info!(mechanism = mounter.name(), "project mount enabled");
        Some(Self::new(mounter, default_node_fs(), settings))
    }

    pub fn settings(&self) -> &MountSettings {
        &self.settings
    }

    fn reindex(&self, entries: &HashMap<PathBuf, Entry>) {
        *self.index.lock().unwrap() = entries.iter().map(|(p, e)| (p.clone(), e.fs.clone())).collect();
    }

    /// Whether `path` is served from a node right now.
    pub fn covers(&self, path: &Path) -> bool {
        self.index.lock().unwrap().iter().any(|(root, _)| path.starts_with(root))
    }

    /// Forget every cached attribute and content (after a command ran on a node).
    pub fn invalidate_all(&self) {
        for (_, fs) in self.index.lock().unwrap().iter() {
            fs.invalidate_all();
        }
    }

    pub async fn list(&self) -> Vec<MountInfo> {
        let entries = self.entries.lock().await;
        let mut out: Vec<MountInfo> = entries
            .iter()
            .map(|(p, e)| MountInfo {
                path: p.clone(),
                computer_id: e.computer_id.clone(),
                sessions: e.users.iter().cloned().collect(),
                outage: e.fs.health().map(|o| o.error),
            })
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    /// Make `root` (the session's cwd) on `computer` available here at the same path, for
    /// `session_id`. Shares an existing mount that covers it.
    pub async fn acquire(&self, session_id: &str, computer: &Computer, root: &Path) -> Result<(), MountError> {
        if !root.is_absolute() {
            return Err(MountError::NotAbsolute(root.into()));
        }
        let mut entries = self.entries.lock().await;

        // Already served?
        let covering = entries
            .iter()
            .find(|(p, _)| root.starts_with(p))
            .map(|(p, e)| (p.clone(), e.computer_id.clone(), e.computer_name.clone()));
        if let Some((path, computer_id, computer_name)) = covering {
            if computer_id != computer.id {
                return Err(MountError::Conflict { path, other: computer_name });
            }
            if let Some(e) = entries.get_mut(&path) {
                e.users.insert(session_id.to_string());
                e.unused_since = None;
            }
            // A session uses one mount at a time.
            self.leave_others(&mut entries, session_id, &path).await;
            return Ok(());
        }
        if let Some(inner) = entries.keys().find(|p| p.starts_with(root)) {
            return Err(MountError::Nested { path: root.into(), other: inner.clone() });
        }
        self.leave_others(&mut entries, session_id, root).await;

        // The directory must exist on the node.
        let node = (self.connect)(computer, self.settings.cache.op_timeout)
            .map_err(|e| MountError::Unreachable { computer: computer.name.clone(), error: format!("{e:#}") })?;
        let fs = Arc::new(RemoteFs::new(node, root.to_path_buf(), self.settings.cache.clone()));
        match fs.getattr(ROOT_ID).await {
            Ok(a) if a.kind == ember_node::proto::FileKind::Dir => {}
            Ok(_) => return Err(MountError::NotADirectory { path: root.into(), computer: computer.name.clone() }),
            Err(FsErr::NoEnt) => {
                return Err(MountError::MissingOnNode { path: root.into(), computer: computer.name.clone() })
            }
            Err(FsErr::Access | FsErr::Perm) => {
                return Err(MountError::Forbidden { path: root.into(), computer: computer.name.clone() })
            }
            Err(e) => return Err(MountError::Unreachable { computer: computer.name.clone(), error: e.to_string() }),
        }

        // Prepare the mount point here.
        let created = self.prepare_mount_point(root).await?;
        match self.mounter.mount(fs.clone(), root).await {
            Ok(active) => {
                tracing::info!(path = %root.display(), computer = %computer.name, mechanism = self.mounter.name(), "project mounted");
                entries.insert(
                    root.to_path_buf(),
                    Entry {
                        computer_id: computer.id.clone(),
                        computer_name: computer.name.clone(),
                        fs,
                        active: Some(active),
                        users: HashSet::from([session_id.to_string()]),
                        created,
                        unused_since: None,
                    },
                );
                self.reindex(&entries);
                Ok(())
            }
            Err(error) => {
                remove_dirs(&created);
                Err(MountError::Mount { path: root.into(), error })
            }
        }
    }

    async fn prepare_mount_point(&self, root: &Path) -> Result<Vec<PathBuf>, MountError> {
        let (os, shadow) = (self.os, self.settings.allow_shadow);
        let inspect = |root: PathBuf| async move {
            tokio::task::spawn_blocking(move || cmd::inspect(os, &root, shadow))
                .await
                .unwrap_or_else(|e| Err(cmd::MountPointError::NotADirectory(PathBuf::from(e.to_string()))))
        };
        let mut state = inspect(root.to_path_buf()).await?;
        if state == MountPoint::Mounted {
            // Left over from a crashed run (we hold no entry for it).
            tracing::warn!(path = %root.display(), "removing a stale mount");
            if let Err(e) = self.mounter.force_unmount(root).await {
                return Err(MountError::Mount { path: root.into(), error: e });
            }
            state = inspect(root.to_path_buf()).await?;
            if state == MountPoint::Mounted {
                return Err(MountError::Mount {
                    path: root.into(),
                    error: anyhow::anyhow!("something else is mounted there"),
                });
            }
        }
        match state {
            MountPoint::Ready | MountPoint::Mounted => Ok(Vec::new()),
            MountPoint::Create(dirs) => {
                let mut created = Vec::new();
                for d in dirs {
                    if let Err(e) = std::fs::create_dir(&d) {
                        if e.kind() != std::io::ErrorKind::AlreadyExists {
                            remove_dirs(&created);
                            return Err(cmd::MountPointError::NeedsSetup {
                                path: root.into(),
                                reason: format!("{}: {e}", d.display()),
                                hint: cmd::setup_hint(self.os, root),
                            }
                            .into());
                        }
                    } else {
                        created.push(d);
                    }
                }
                Ok(created)
            }
        }
    }

    /// Remove `session_id` from every mount other than `keep`, unmounting those left unused.
    async fn leave_others(&self, entries: &mut HashMap<PathBuf, Entry>, session_id: &str, keep: &Path) {
        let mut empty = Vec::new();
        for (p, e) in entries.iter_mut() {
            if p != keep && e.users.remove(session_id) && e.users.is_empty() {
                empty.push(p.clone());
            }
        }
        for p in empty {
            self.unmount_entry(entries, &p).await;
        }
    }

    async fn unmount_entry(&self, entries: &mut HashMap<PathBuf, Entry>, path: &Path) {
        let Some(mut e) = entries.remove(path) else { return };
        if let Some(active) = e.active.take() {
            match active.unmount().await {
                Ok(()) => tracing::info!(path = %path.display(), "project unmounted"),
                Err(err) => tracing::warn!(path = %path.display(), "unmounting failed: {err:#}"),
            }
        }
        remove_dirs(&e.created);
        self.reindex(entries);
    }

    /// The session no longer needs its mount (switched away, deleted). Unmounts at once when it
    /// was the last user.
    pub async fn release(&self, session_id: &str) {
        let mut entries = self.entries.lock().await;
        self.leave_others(&mut entries, session_id, Path::new("")).await;
        self.notified.lock().unwrap().remove(session_id);
    }

    /// Unmount mounts none of whose sessions has had a live agent for longer than the grace
    /// period (idle release, exit, deletion). Call periodically. Users are not dropped here: a
    /// session between mounting and its agent starting is not live yet.
    pub async fn sweep<F, Fut>(&self, is_live: F)
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let mut entries = self.entries.lock().await;
        let now = SystemTime::now();
        let mut expired = Vec::new();
        for (p, e) in entries.iter_mut() {
            let mut any_live = false;
            for u in e.users.iter() {
                if is_live(u.clone()).await {
                    any_live = true;
                    break;
                }
            }
            if any_live {
                e.unused_since = None;
                continue;
            }
            let since = *e.unused_since.get_or_insert(now);
            if now.duration_since(since).unwrap_or_default() >= self.settings.idle_grace {
                expired.push(p.clone());
            }
        }
        for p in expired {
            self.unmount_entry(&mut entries, &p).await;
        }
    }

    /// Unmount everything (server shutdown).
    pub async fn shutdown(&self) {
        let mut entries = self.entries.lock().await;
        let all: Vec<PathBuf> = entries.keys().cloned().collect();
        for p in all {
            self.unmount_entry(&mut entries, &p).await;
        }
    }

    /// A one-time notice for the session when its mount's computer is unreachable.
    pub fn outage_notice(&self, session_id: &str) -> Option<String> {
        // `try_lock`: never wait on a mount in progress from a sync hook.
        let entries = self.entries.try_lock().ok()?;
        let (path, e) = entries.iter().find(|(_, e)| e.users.contains(session_id))?;
        let outage = e.fs.health()?;
        let mut notified = self.notified.lock().unwrap();
        if notified.get(session_id) == Some(&outage.since) {
            return None;
        }
        notified.insert(session_id.to_string(), outage.since);
        Some(format!(
            "[Ember system notice] The project files at {} on computer \"{}\" are unreachable right now \
             ({}). Read, Edit, Write, Glob and Grep on them fail with I/O or timeout errors until the \
             computer is back; do not retry in a loop — tell the user.",
            path.display(),
            e.computer_name,
            outage.error
        ))
    }
}

fn remove_dirs(created: &[PathBuf]) {
    for d in created.iter().rev() {
        let _ = std::fs::remove_dir(d);
    }
}

// ---------------------------------------------------------------------------------------------
// Control socket for the `ember-exec` shim

/// A private datagram socket the shim notifies after each remote command (`invalidate`).
pub struct CtlSocket {
    pub path: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl CtlSocket {
    /// Bind `<dir>/ctl.sock` in a fresh 0700 directory under the temp dir.
    pub fn start(mounts: std::sync::Weak<ProjectMounts>) -> anyhow::Result<CtlSocket> {
        use std::os::unix::fs::DirBuilderExt;
        let dir = std::env::temp_dir().join(format!("ember-mount-{}", uuid::Uuid::new_v4().simple()));
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let path = dir.join("ctl.sock");
        let sock = tokio::net::UnixDatagram::bind(&path)?;
        let task = tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                let Ok(n) = sock.recv(&mut buf).await else { break };
                let Some(mounts) = mounts.upgrade() else { break };
                if buf[..n].starts_with(b"invalidate") {
                    mounts.invalidate_all();
                }
            }
        });
        Ok(CtlSocket { path, task })
    }
}

impl Drop for CtlSocket {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
        if let Some(d) = self.path.parent() {
            let _ = std::fs::remove_dir(d);
        }
    }
}

/// Shim side: tell the server that a command ran (best effort, never blocks).
pub fn notify_invalidate(ctl: &Path) {
    if let Ok(s) = std::os::unix::net::UnixDatagram::unbound() {
        let _ = s.set_nonblocking(true);
        let _ = s.send_to(b"invalidate", ctl);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::remote_fs::tests::FakeNode;
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Records mounts instead of calling the kernel.
    #[derive(Default)]
    struct FakeMounter {
        mounted: Arc<Mutex<Vec<PathBuf>>>,
        unmounts: Arc<AtomicUsize>,
        fail: bool,
    }

    struct FakeActive {
        at: PathBuf,
        mounted: Arc<Mutex<Vec<PathBuf>>>,
        unmounts: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ActiveMount for FakeActive {
        async fn unmount(self: Box<Self>) -> anyhow::Result<()> {
            self.mounted.lock().unwrap().retain(|p| p != &self.at);
            self.unmounts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[async_trait]
    impl Mounter for FakeMounter {
        fn name(&self) -> &'static str {
            "fake"
        }

        async fn mount(&self, _fs: Arc<RemoteFs>, at: &Path) -> anyhow::Result<Box<dyn ActiveMount>> {
            anyhow::ensure!(!self.fail, "mount_nfs: permission denied");
            assert!(at.is_dir(), "the mount point exists before mounting");
            self.mounted.lock().unwrap().push(at.into());
            Ok(Box::new(FakeActive { at: at.into(), mounted: self.mounted.clone(), unmounts: self.unmounts.clone() }))
        }

        async fn force_unmount(&self, _at: &Path) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn computer(id: &str) -> Computer {
        Computer { id: id.into(), name: format!("{id}-name"), url: "http://x:1".into(), token: "t".into(), created_at: 0, peer: None }
    }

    /// Project mounts over a fake node and a fake mounter; `base` is a temp directory that
    /// exists on both, with `proj`, `proj/sub` and `other` only on the node.
    pub(crate) struct Fx {
        pub(crate) mounts: Arc<ProjectMounts>,
        pub(crate) mounted: Arc<Mutex<Vec<PathBuf>>>,
        pub(crate) unmounts: Arc<AtomicUsize>,
        pub(crate) node: Arc<FakeNode>,
        pub(crate) base: PathBuf,
        _dir: tempfile::TempDir,
    }

    pub(crate) fn fx(fail: bool) -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let node = FakeNode::new(base.to_str().unwrap());
        // The node has the project dirs; the server (the tempdir) does not, except `base`.
        for p in ["proj", "proj/sub", "other"] {
            node.put_dir(base.join(p).to_str().unwrap());
        }
        let m = FakeMounter { fail, ..Default::default() };
        let (mounted, unmounts) = (m.mounted.clone(), m.unmounts.clone());
        let n2 = node.clone();
        let connect: NodeFsConnector = Arc::new(move |c: &Computer, _t: Duration| {
            anyhow::ensure!(c.id != "down", "connection refused");
            Ok(n2.clone() as Arc<dyn NodeFs>)
        });
        let mut settings = MountSettings::from_vars(|_| None);
        settings.idle_grace = Duration::ZERO;
        // Long TTL: tests see changes only through explicit invalidation.
        settings.cache.attr_ttl = Duration::from_secs(60);
        Fx { mounts: ProjectMounts::new(Arc::new(m), connect, settings), mounted, unmounts, node, base, _dir: dir }
    }

    #[tokio::test]
    async fn mounts_at_the_same_path_shares_and_unmounts_when_unused() {
        let f = fx(false);
        let proj = f.base.join("proj");
        let pi = computer("pi");
        f.mounts.acquire("s1", &pi, &proj).await.unwrap();
        assert_eq!(*f.mounted.lock().unwrap(), std::slice::from_ref(&proj));
        assert!(proj.is_dir(), "the mount point was created");
        assert!(f.mounts.covers(&proj.join("src/main.rs")));
        // A second session in the same project (or below it) shares the mount.
        f.mounts.acquire("s2", &pi, &proj.join("sub")).await.unwrap();
        assert_eq!(f.mounted.lock().unwrap().len(), 1);

        f.mounts.release("s1").await;
        assert_eq!(f.unmounts.load(Ordering::SeqCst), 0, "s2 still uses it");
        f.mounts.release("s2").await;
        assert_eq!(f.unmounts.load(Ordering::SeqCst), 1);
        assert!(!proj.exists(), "the created mount point is removed again");
        assert!(!f.mounts.covers(&proj));
    }

    #[tokio::test]
    async fn conflicts_and_nesting_are_refused() {
        let f = fx(false);
        let proj = f.base.join("proj");
        f.mounts.acquire("s1", &computer("pi"), &proj).await.unwrap();
        let e = f.mounts.acquire("s2", &computer("mac"), &proj).await.unwrap_err();
        assert!(matches!(e, MountError::Conflict { .. }), "{e}");
        let e = f.mounts.acquire("s3", &computer("pi"), &f.base).await.unwrap_err();
        assert!(matches!(e, MountError::Nested { .. }), "{e}");
        let e = f.mounts.acquire("s4", &computer("pi"), Path::new("rel")).await.unwrap_err();
        assert!(matches!(e, MountError::NotAbsolute(_)));
    }

    #[tokio::test]
    async fn moving_a_session_to_another_project_releases_the_old_mount() {
        let f = fx(false);
        let pi = computer("pi");
        f.mounts.acquire("s1", &pi, &f.base.join("proj")).await.unwrap();
        f.mounts.acquire("s1", &pi, &f.base.join("other")).await.unwrap();
        assert_eq!(*f.mounted.lock().unwrap(), [f.base.join("other")]);
        assert_eq!(f.unmounts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn clear_errors_for_missing_dirs_unreachable_nodes_and_failed_mounts() {
        let f = fx(false);
        let e = f.mounts.acquire("s", &computer("pi"), &f.base.join("nope")).await.unwrap_err();
        assert!(matches!(e, MountError::MissingOnNode { .. }), "{e}");
        assert!(e.to_string().contains("does not exist on computer \"pi-name\""), "{e}");
        let e = f.mounts.acquire("s", &computer("down"), &f.base.join("proj")).await.unwrap_err();
        assert!(e.to_string().contains("\"down-name\" is unreachable"), "{e}");
        f.node.down.store(true, Ordering::SeqCst);
        let e = f.mounts.acquire("s", &computer("pi"), &f.base.join("proj")).await.unwrap_err();
        assert!(matches!(e, MountError::Unreachable { .. }), "{e}");

        let f = fx(true);
        let e = f.mounts.acquire("s", &computer("pi"), &f.base.join("proj")).await.unwrap_err();
        assert!(e.to_string().contains("permission denied"), "{e}");
        assert!(!f.base.join("proj").exists(), "a failed mount leaves no directory behind");
    }

    #[tokio::test]
    async fn a_non_empty_local_directory_is_not_hidden() {
        let f = fx(false);
        let proj = f.base.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("local.txt"), "mine").unwrap();
        let e = f.mounts.acquire("s", &computer("pi"), &proj).await.unwrap_err();
        assert!(e.to_string().contains("EMBER_MOUNT_SHADOW"), "{e}");
        assert!(f.mounted.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn sweep_unmounts_when_no_session_is_live() {
        let f = fx(false);
        f.mounts.acquire("live", &computer("pi"), &f.base.join("proj")).await.unwrap();
        f.mounts.acquire("dead", &computer("pi"), &f.base.join("proj")).await.unwrap();
        f.mounts.sweep(|s| async move { s == "live" }).await;
        assert_eq!(f.unmounts.load(Ordering::SeqCst), 0, "one live session keeps it");
        assert_eq!(f.mounts.list().await[0].sessions.len(), 2);
        f.mounts.sweep(|_| async { false }).await;
        assert_eq!(f.unmounts.load(Ordering::SeqCst), 1, "no live session for the (zero) grace period");
        assert!(f.mounts.list().await.is_empty());
    }

    #[tokio::test]
    async fn outage_is_reported_once_per_outage() {
        let f = fx(false);
        let proj = f.base.join("proj");
        f.mounts.acquire("s", &computer("pi"), &proj).await.unwrap();
        assert!(f.mounts.outage_notice("s").is_none());
        f.node.down.store(true, Ordering::SeqCst);
        // Any file access through the mount notices the outage.
        let fs = f.mounts.index.lock().unwrap()[0].1.clone();
        fs.invalidate_all();
        assert!(fs.getattr(ROOT_ID).await.is_err());
        let n = f.mounts.outage_notice("s").unwrap();
        assert!(n.contains("unreachable") && n.contains("pi-name"), "{n}");
        assert!(f.mounts.outage_notice("s").is_none(), "only once");
        assert!(f.mounts.outage_notice("other").is_none());
    }

    #[tokio::test]
    async fn ctl_socket_invalidates_caches() {
        let f = fx(false);
        let proj = f.base.join("proj");
        f.node.put_file(proj.join("a").to_str().unwrap(), b"1");
        f.mounts.acquire("s", &computer("pi"), &proj).await.unwrap();
        let fs = f.mounts.index.lock().unwrap()[0].1.clone();
        let a = fs.lookup(ROOT_ID, std::ffi::OsStr::new("a")).await.unwrap();
        assert_eq!(fs.read(a.id, 0, 10).await.unwrap().0, b"1");
        f.node.put_file(proj.join("a").to_str().unwrap(), b"2");

        let ctl = CtlSocket::start(Arc::downgrade(&f.mounts)).unwrap();
        notify_invalidate(&ctl.path);
        let mut seen = Vec::new();
        for _ in 0..50 {
            seen = fs.read(a.id, 0, 10).await.unwrap().0;
            if seen == b"2" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(seen, b"2");
    }

    #[test]
    fn settings_from_env() {
        let s = MountSettings::from_vars(|k| match k {
            ENV_MOUNT => Some("off".into()),
            ENV_TTL_MS => Some("250".into()),
            ENV_SHADOW => Some("1".into()),
            _ => None,
        });
        assert_eq!(s.mechanism, Mechanism::Off);
        assert_eq!(s.cache.attr_ttl, Duration::from_millis(250));
        assert!(s.allow_shadow);
        assert!(s.cache.op_timeout < cmd::NfsOptions::new(1).worst_case_wait());
        assert_eq!(MountSettings::from_vars(|k| (k == ENV_MOUNT).then(|| "nfs".into())).mechanism, Mechanism::Nfs);
    }
}
