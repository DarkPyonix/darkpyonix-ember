//! The protocol-neutral half of the project mount: a filesystem over the ember node file API with
//! an inode table and short-lived caches. The NFS ([`super::nfs`]) and FUSE ([`super::fuse`])
//! front ends are thin translations of this.
//!
//! - **Inodes** are this mount's own numbers, one per node path seen, stable for the mount's
//!   lifetime (renames move them). `1` is the root. Node inode numbers are not used: a root may
//!   span several devices on the node, and the front ends need the id → path map anyway.
//! - **Attributes and directory listings** are cached for [`CacheConfig::attr_ttl`]; a listing
//!   also fills the attribute cache (one round trip for `ls -l` / Glob). Every change made
//!   through the mount updates or drops the entries it affects.
//! - **File contents** up to [`CacheConfig::content_max`] are fetched whole on first read and
//!   served from memory while the file's size and mtime are unchanged; larger files are read by
//!   range.
//! - **Changes made on the node by anything else** (the agent's Bash tool through `ember-exec`,
//!   the user's editor) become visible after the TTL, or at once when [`RemoteFs::invalidate_all`]
//!   is called; the `ember-exec` shim asks for that after every command (see
//!   `super::CtlSocket`). The node has no change feed yet.
//! - **Every node call has a deadline** ([`CacheConfig::op_timeout`]); a call that misses it, or
//!   cannot reach the node, fails with [`FsErr::TimedOut`] / [`FsErr::Unreachable`] (EIO /
//!   ETIMEDOUT to the agent) instead of hanging, and is recorded in [`RemoteFs::health`].

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use ember_node::client::{ClientError, NodeClient};
use ember_node::proto::{
    ErrorCode, Expect, FileKind, MkdirRequest, ReadRequest, RenameRequest, SetAttrRequest, SetTime, Stat,
    WriteRequest,
};

/// The root's inode number.
pub const ROOT_ID: u64 = 1;

/// Errors as the front ends need them (each maps to an NFS status and an errno).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FsErr {
    #[error("no such file or directory")]
    NoEnt,
    #[error("file exists")]
    Exist,
    #[error("not a directory")]
    NotDir,
    #[error("is a directory")]
    IsDir,
    #[error("directory not empty")]
    NotEmpty,
    #[error("permission denied")]
    Access,
    #[error("operation not permitted")]
    Perm,
    #[error("invalid argument")]
    Inval,
    #[error("file name too long")]
    NameTooLong,
    #[error("no space left on device")]
    NoSpc,
    #[error("read-only file system")]
    Rofs,
    #[error("resource busy")]
    Busy,
    #[error("cross-device link")]
    XDev,
    #[error("too many levels of symbolic links")]
    Loop,
    #[error("not supported")]
    NotSupp,
    /// The inode is not (or no longer) known to this mount.
    #[error("stale file handle")]
    Stale,
    /// The node answered with an error that has no closer match.
    #[error("node error: {0}")]
    Io(String),
    /// The node could not be reached at all.
    #[error("computer unreachable: {0}")]
    Unreachable(String),
    /// The node did not answer within the deadline.
    #[error("computer did not answer in time")]
    TimedOut,
}

impl FsErr {
    /// From the node's portable errno name ([`ember_node::proto::ErrorBody::errno`]).
    pub fn from_errno_name(name: &str) -> Option<FsErr> {
        Some(match name {
            "ENOENT" => FsErr::NoEnt,
            "EEXIST" => FsErr::Exist,
            "ENOTDIR" => FsErr::NotDir,
            "EISDIR" => FsErr::IsDir,
            "ENOTEMPTY" => FsErr::NotEmpty,
            "EACCES" => FsErr::Access,
            "EPERM" => FsErr::Perm,
            "EINVAL" => FsErr::Inval,
            "ENAMETOOLONG" => FsErr::NameTooLong,
            "ENOSPC" | "EDQUOT" => FsErr::NoSpc,
            "EROFS" => FsErr::Rofs,
            "EBUSY" | "ETXTBSY" => FsErr::Busy,
            "EXDEV" => FsErr::XDev,
            "ELOOP" => FsErr::Loop,
            "ENOTSUP" => FsErr::NotSupp,
            _ => return None,
        })
    }

    /// The local errno (for FUSE).
    pub fn errno(&self) -> i32 {
        match self {
            FsErr::NoEnt => libc::ENOENT,
            FsErr::Exist => libc::EEXIST,
            FsErr::NotDir => libc::ENOTDIR,
            FsErr::IsDir => libc::EISDIR,
            FsErr::NotEmpty => libc::ENOTEMPTY,
            FsErr::Access => libc::EACCES,
            FsErr::Perm => libc::EPERM,
            FsErr::Inval => libc::EINVAL,
            FsErr::NameTooLong => libc::ENAMETOOLONG,
            FsErr::NoSpc => libc::ENOSPC,
            FsErr::Rofs => libc::EROFS,
            FsErr::Busy => libc::EBUSY,
            FsErr::XDev => libc::EXDEV,
            FsErr::Loop => libc::ELOOP,
            FsErr::NotSupp => libc::ENOTSUP,
            FsErr::Stale => libc::ESTALE,
            FsErr::Io(_) | FsErr::Unreachable(_) => libc::EIO,
            FsErr::TimedOut => libc::ETIMEDOUT,
        }
    }

    /// Whether this says the node is unhealthy (rather than refusing one operation).
    pub fn is_outage(&self) -> bool {
        matches!(self, FsErr::Unreachable(_) | FsErr::TimedOut)
    }
}

impl From<ClientError> for FsErr {
    fn from(e: ClientError) -> FsErr {
        // The client's deadline ([`NodeClient::with_deadline`], over HTTP or the transport).
        if e.is_timeout() {
            return FsErr::TimedOut;
        }
        // Includes dial / stream failures over the peer-to-peer transport.
        if e.is_transport() {
            return FsErr::Unreachable(e.to_string());
        }
        if let Some(f) = e.errno().and_then(FsErr::from_errno_name) {
            return f;
        }
        match e.code() {
            Some(ErrorCode::NotFound) => FsErr::NoEnt,
            Some(ErrorCode::ForbiddenPath) => FsErr::Access,
            Some(ErrorCode::PreconditionFailed) => FsErr::Exist,
            Some(ErrorCode::Unauthorized) => FsErr::Perm,
            _ => FsErr::Io(e.to_string()),
        }
    }
}

pub type FsResult<T> = Result<T, FsErr>;

/// The node file operations the mount uses. Implemented by [`NodeClient`]; tests use a fake.
#[async_trait]
pub trait NodeFs: Send + Sync {
    /// Describe `path` without following a final symbolic link.
    async fn lstat(&self, path: &Path) -> FsResult<Stat>;
    async fn list(&self, path: &Path) -> FsResult<Vec<ember_node::proto::DirEntry>>;
    /// Up to `len` bytes from `offset` (`None` = as much as one call returns), and the file's
    /// size and mtime at the time of the read.
    async fn read(&self, path: &Path, offset: u64, len: Option<u64>) -> FsResult<ReadChunk>;
    async fn pwrite(&self, path: &Path, offset: u64, data: &[u8]) -> FsResult<Stat>;
    /// Create an empty regular file; `EEXIST` if anything exists at `path`.
    async fn create_new(&self, path: &Path) -> FsResult<()>;
    async fn mkdir(&self, path: &Path, mode: Option<u32>) -> FsResult<Stat>;
    async fn remove(&self, path: &Path) -> FsResult<()>;
    async fn rename(&self, from: &Path, to: &Path, overwrite: bool) -> FsResult<()>;
    async fn symlink(&self, path: &Path, target: &Path) -> FsResult<Stat>;
    async fn readlink(&self, path: &Path) -> FsResult<PathBuf>;
    async fn setattr(&self, req: &SetAttrRequest) -> FsResult<Stat>;
}

#[derive(Debug, Clone)]
pub struct ReadChunk {
    pub data: Vec<u8>,
    pub size: u64,
    pub mtime_ms: Option<u64>,
    pub eof: bool,
}

#[async_trait]
impl NodeFs for NodeClient {
    async fn lstat(&self, path: &Path) -> FsResult<Stat> {
        Ok(NodeClient::lstat(self, path).await?)
    }

    async fn list(&self, path: &Path) -> FsResult<Vec<ember_node::proto::DirEntry>> {
        Ok(NodeClient::list(self, path).await?.entries)
    }

    async fn read(&self, path: &Path, offset: u64, len: Option<u64>) -> FsResult<ReadChunk> {
        let r = NodeClient::read(self, &ReadRequest { path: path.into(), offset, len, hash: false }).await?;
        Ok(ReadChunk { data: r.data, size: r.size, mtime_ms: r.mtime_ms, eof: r.eof })
    }

    async fn pwrite(&self, path: &Path, offset: u64, data: &[u8]) -> FsResult<Stat> {
        Ok(NodeClient::pwrite(self, path, offset, data.to_vec()).await?)
    }

    async fn create_new(&self, path: &Path) -> FsResult<()> {
        let req = WriteRequest { path: path.into(), data: Vec::new(), expect: Some(Expect::Absent), create_parents: false };
        NodeClient::write(self, &req).await?;
        Ok(())
    }

    async fn mkdir(&self, path: &Path, mode: Option<u32>) -> FsResult<Stat> {
        Ok(NodeClient::mkdir(self, &MkdirRequest { path: path.into(), parents: false, mode }).await?)
    }

    async fn remove(&self, path: &Path) -> FsResult<()> {
        Ok(NodeClient::remove(self, path, false).await?)
    }

    async fn rename(&self, from: &Path, to: &Path, overwrite: bool) -> FsResult<()> {
        Ok(NodeClient::rename(self, &RenameRequest { from: from.into(), to: to.into(), overwrite }).await?)
    }

    async fn symlink(&self, path: &Path, target: &Path) -> FsResult<Stat> {
        Ok(NodeClient::symlink(self, path, target).await?)
    }

    async fn readlink(&self, path: &Path) -> FsResult<PathBuf> {
        Ok(NodeClient::readlink(self, path).await?.target)
    }

    async fn setattr(&self, req: &SetAttrRequest) -> FsResult<Stat> {
        Ok(NodeClient::setattr(self, req).await?)
    }
}

/// File attributes as the front ends report them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attr {
    pub id: u64,
    pub kind: FileKind,
    pub size: u64,
    /// Permission bits only (no file type bits).
    pub perm: u32,
    pub nlink: u32,
    pub mtime_ms: u64,
    pub atime_ms: u64,
    pub ctime_ms: u64,
}

impl Attr {
    fn of(id: u64, s: &Stat) -> Attr {
        let mtime = s.mtime_ms.unwrap_or(0);
        Attr {
            id,
            kind: s.kind,
            size: s.size,
            perm: s.mode & 0o7777,
            nlink: s.nlink.map(|n| n.min(u32::MAX as u64) as u32).unwrap_or(1),
            mtime_ms: mtime,
            atime_ms: s.atime_ms.unwrap_or(mtime),
            ctime_ms: s.ctime_ms.unwrap_or(mtime),
        }
    }

    fn of_entry(id: u64, e: &ember_node::proto::DirEntry) -> Attr {
        let mtime = e.mtime_ms.unwrap_or(0);
        let default_perm = if e.kind == FileKind::Dir { 0o755 } else { 0o644 };
        Attr {
            id,
            kind: e.kind,
            size: e.size,
            perm: e.mode.map(|m| m & 0o7777).unwrap_or(default_perm),
            nlink: 1,
            mtime_ms: mtime,
            atime_ms: mtime,
            ctime_ms: mtime,
        }
    }

    pub fn time(ms: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(ms)
    }
}

/// Attribute changes (what NFS SETATTR / FUSE setattr carry that the node can apply).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SetAttr {
    pub perm: Option<u32>,
    pub size: Option<u64>,
    pub atime: Option<SetTime>,
    pub mtime: Option<SetTime>,
}

impl SetAttr {
    fn is_empty(&self) -> bool {
        self.perm.is_none() && self.size.is_none() && self.atime.is_none() && self.mtime.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// How long attributes and listings are trusted without asking the node.
    pub attr_ttl: Duration,
    /// Deadline for one node call.
    pub op_timeout: Duration,
    /// Files up to this size are fetched whole and kept in memory.
    pub content_max: u64,
    /// Total bytes of cached file contents.
    pub content_budget: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        CacheConfig {
            attr_ttl: Duration::from_secs(1),
            op_timeout: Duration::from_secs(4),
            content_max: 4 * 1024 * 1024,
            content_budget: 64 * 1024 * 1024,
        }
    }
}

/// Last outage seen by a mount, for telling the agent and the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outage {
    pub since: SystemTime,
    pub error: String,
}

#[derive(Default)]
struct Inodes {
    by_id: HashMap<u64, PathBuf>,
    by_path: HashMap<PathBuf, u64>,
    next: u64,
}

impl Inodes {
    fn new(root: &Path) -> Inodes {
        let mut i = Inodes { next: ROOT_ID + 1, ..Default::default() };
        i.by_id.insert(ROOT_ID, root.to_path_buf());
        i.by_path.insert(root.to_path_buf(), ROOT_ID);
        i
    }

    fn id_for(&mut self, path: &Path) -> u64 {
        if let Some(id) = self.by_path.get(path) {
            return *id;
        }
        let id = self.next;
        self.next += 1;
        self.by_id.insert(id, path.to_path_buf());
        self.by_path.insert(path.to_path_buf(), id);
        id
    }

    /// Forget `path` and everything under it; returns the ids dropped.
    fn forget_tree(&mut self, path: &Path) -> Vec<u64> {
        let gone: Vec<PathBuf> = self.by_path.keys().filter(|p| p.starts_with(path)).cloned().collect();
        let mut ids = Vec::new();
        for p in gone {
            if let Some(id) = self.by_path.remove(&p) {
                if id != ROOT_ID {
                    self.by_id.remove(&id);
                    ids.push(id);
                } else {
                    self.by_path.insert(p, id);
                }
            }
        }
        ids
    }

    /// Move `from` and everything under it to `to`, keeping their ids.
    fn rename_tree(&mut self, from: &Path, to: &Path) {
        let moved: Vec<(PathBuf, u64)> = self
            .by_path
            .iter()
            .filter(|(p, _)| p.starts_with(from))
            .map(|(p, id)| (p.clone(), *id))
            .collect();
        for (p, _) in &moved {
            self.by_path.remove(p);
        }
        for (p, id) in moved {
            let rest = p.strip_prefix(from).expect("filtered by prefix");
            let np = if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) };
            self.by_id.insert(id, np.clone());
            self.by_path.insert(np, id);
        }
    }
}

struct Listing {
    /// `(name, id)`, sorted by name.
    entries: Vec<(OsString, u64)>,
    at: Instant,
}

struct Content {
    data: Arc<Vec<u8>>,
    size: u64,
    mtime_ms: u64,
}

#[derive(Default)]
struct Caches {
    attrs: HashMap<u64, (Attr, Instant)>,
    listings: HashMap<u64, Listing>,
    contents: HashMap<u64, Content>,
    content_bytes: u64,
}

impl Caches {
    fn drop_id(&mut self, id: u64) {
        self.attrs.remove(&id);
        self.listings.remove(&id);
        if let Some(c) = self.contents.remove(&id) {
            self.content_bytes -= c.data.len() as u64;
        }
    }
}

/// A node directory tree as a filesystem. Cheap to share (`Arc`).
pub struct RemoteFs {
    node: Arc<dyn NodeFs>,
    root: PathBuf,
    cfg: CacheConfig,
    inodes: Mutex<Inodes>,
    caches: Mutex<Caches>,
    outage: Mutex<Option<Outage>>,
}

impl RemoteFs {
    /// `root` is the absolute node path served as the filesystem's root.
    pub fn new(node: Arc<dyn NodeFs>, root: PathBuf, cfg: CacheConfig) -> RemoteFs {
        RemoteFs {
            inodes: Mutex::new(Inodes::new(&root)),
            node,
            root,
            cfg,
            caches: Mutex::new(Caches::default()),
            outage: Mutex::new(None),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The last outage, while it lasts (cleared by the next successful node call).
    pub fn health(&self) -> Option<Outage> {
        self.outage.lock().unwrap().clone()
    }

    /// Drop every cached attribute, listing and content (the inode table is kept).
    pub fn invalidate_all(&self) {
        let mut c = self.caches.lock().unwrap();
        *c = Caches::default();
    }

    /// Run one node call under the deadline and record the outcome in [`RemoteFs::health`].
    async fn call<T>(&self, fut: impl std::future::Future<Output = FsResult<T>>) -> FsResult<T> {
        let res = match tokio::time::timeout(self.cfg.op_timeout, fut).await {
            Ok(r) => r,
            Err(_) => Err(FsErr::TimedOut),
        };
        let mut o = self.outage.lock().unwrap();
        match &res {
            Err(e) if e.is_outage() => {
                if o.is_none() {
                    tracing::warn!(root = %self.root.display(), "project mount: {e}");
                    *o = Some(Outage { since: SystemTime::now(), error: e.to_string() });
                }
            }
            _ => {
                if o.take().is_some() {
                    tracing::info!(root = %self.root.display(), "project mount: computer reachable again");
                }
            }
        }
        res
    }

    pub fn path_of(&self, id: u64) -> FsResult<PathBuf> {
        self.inodes.lock().unwrap().by_id.get(&id).cloned().ok_or(FsErr::Stale)
    }

    fn child_path(&self, dir: u64, name: &OsStr) -> FsResult<PathBuf> {
        let bytes = name.as_bytes();
        if bytes.is_empty() || bytes.contains(&b'/') || bytes.contains(&0) || name == "." || name == ".." {
            return Err(FsErr::Inval);
        }
        if bytes.len() > 255 {
            return Err(FsErr::NameTooLong);
        }
        Ok(self.path_of(dir)?.join(name))
    }

    fn remember(&self, path: &Path, stat: &Stat) -> Attr {
        let id = self.inodes.lock().unwrap().id_for(path);
        let attr = Attr::of(id, stat);
        self.put_attr(attr);
        attr
    }

    fn put_attr(&self, attr: Attr) {
        let mut c = self.caches.lock().unwrap();
        // Drop cached contents that no longer match.
        if let Some(content) = c.contents.get(&attr.id) {
            if content.size != attr.size || content.mtime_ms != attr.mtime_ms {
                let n = content.data.len() as u64;
                c.contents.remove(&attr.id);
                c.content_bytes -= n;
            }
        }
        c.attrs.insert(attr.id, (attr, Instant::now()));
    }

    fn fresh_attr(&self, id: u64) -> Option<Attr> {
        let c = self.caches.lock().unwrap();
        c.attrs.get(&id).filter(|(_, at)| at.elapsed() < self.cfg.attr_ttl).map(|(a, _)| *a)
    }

    /// A directory changed through the mount: its listing and attributes are stale.
    fn dir_changed(&self, dir: u64) {
        let mut c = self.caches.lock().unwrap();
        c.listings.remove(&dir);
        c.attrs.remove(&dir);
    }

    fn parent_id(&self, path: &Path) -> Option<u64> {
        let parent = path.parent()?;
        self.inodes.lock().unwrap().by_path.get(parent).copied()
    }

    // -----------------------------------------------------------------------------------------
    // Operations

    pub async fn getattr(&self, id: u64) -> FsResult<Attr> {
        if let Some(a) = self.fresh_attr(id) {
            return Ok(a);
        }
        let path = self.path_of(id)?;
        match self.call(self.node.lstat(&path)).await {
            Ok(s) => {
                let a = Attr::of(id, &s);
                self.put_attr(a);
                Ok(a)
            }
            Err(FsErr::NoEnt) if id != ROOT_ID => {
                // Gone on the node: the handle is stale.
                let mut i = self.inodes.lock().unwrap();
                for gone in i.forget_tree(&path) {
                    self.caches.lock().unwrap().drop_id(gone);
                }
                Err(FsErr::NoEnt)
            }
            Err(e) => Err(e),
        }
    }

    pub async fn lookup(&self, dir: u64, name: &OsStr) -> FsResult<Attr> {
        if name == "." {
            return self.getattr(dir).await;
        }
        if name == ".." {
            let path = self.path_of(dir)?;
            let parent = if dir == ROOT_ID { ROOT_ID } else { self.parent_id(&path).unwrap_or(ROOT_ID) };
            return self.getattr(parent).await;
        }
        let path = self.child_path(dir, name)?;
        // A fresh listing answers lookups (including negative ones) without a round trip.
        let listed = {
            let c = self.caches.lock().unwrap();
            c.listings
                .get(&dir)
                .filter(|l| l.at.elapsed() < self.cfg.attr_ttl)
                .map(|l| l.entries.iter().find(|(n, _)| n == name).map(|(_, id)| *id))
        };
        match listed {
            Some(None) => return Err(FsErr::NoEnt),
            Some(Some(id)) => {
                if let Some(a) = self.fresh_attr(id) {
                    return Ok(a);
                }
            }
            None => {}
        }
        let s = self.call(self.node.lstat(&path)).await?;
        Ok(self.remember(&path, &s))
    }

    /// The directory's entries (without `.` and `..`), sorted by name, with attributes.
    pub async fn readdir(&self, dir: u64) -> FsResult<Vec<(OsString, Attr)>> {
        let cached = {
            let c = self.caches.lock().unwrap();
            c.listings.get(&dir).filter(|l| l.at.elapsed() < self.cfg.attr_ttl).map(|l| {
                l.entries
                    .iter()
                    .map(|(n, id)| (n.clone(), c.attrs.get(id).map(|(a, _)| *a)))
                    .collect::<Vec<_>>()
            })
        };
        if let Some(entries) = cached {
            if entries.iter().all(|(_, a)| a.is_some()) {
                return Ok(entries.into_iter().map(|(n, a)| (n, a.expect("checked"))).collect());
            }
        }
        let path = self.path_of(dir)?;
        let listed = self.call(self.node.list(&path)).await?;
        let mut out = Vec::with_capacity(listed.len());
        let mut ids = Vec::with_capacity(listed.len());
        {
            let mut i = self.inodes.lock().unwrap();
            for e in &listed {
                let name = OsString::from(&e.name);
                let id = i.id_for(&path.join(&name));
                ids.push((name, id));
            }
        }
        for (e, (name, id)) in listed.iter().zip(&ids) {
            let attr = Attr::of_entry(*id, e);
            self.put_attr(attr);
            out.push((name.clone(), attr));
        }
        // Names the node no longer has are gone: forget their inodes.
        {
            let now: HashSet<&OsString> = ids.iter().map(|(n, _)| n).collect();
            let old = self.caches.lock().unwrap().listings.remove(&dir);
            if let Some(old) = old {
                for (n, _) in old.entries.iter().filter(|(n, _)| !now.contains(n)) {
                    let gone = self.inodes.lock().unwrap().forget_tree(&path.join(n));
                    let mut c = self.caches.lock().unwrap();
                    for id in gone {
                        c.drop_id(id);
                    }
                }
            }
        }
        self.caches.lock().unwrap().listings.insert(dir, Listing { entries: ids, at: Instant::now() });
        Ok(out)
    }

    /// Up to `count` bytes at `offset`; the flag is true at end of file.
    pub async fn read(&self, id: u64, offset: u64, count: u32) -> FsResult<(Vec<u8>, bool)> {
        let attr = self.getattr(id).await?;
        match attr.kind {
            FileKind::Dir => return Err(FsErr::IsDir),
            FileKind::File => {}
            _ => return Err(FsErr::Inval),
        }
        let slice = |data: &[u8]| -> (Vec<u8>, bool) {
            let start = (offset as usize).min(data.len());
            let end = start.saturating_add(count as usize).min(data.len());
            (data[start..end].to_vec(), end >= data.len())
        };
        {
            let c = self.caches.lock().unwrap();
            if let Some(content) = c.contents.get(&id) {
                if content.size == attr.size && content.mtime_ms == attr.mtime_ms {
                    return Ok(slice(&content.data));
                }
            }
        }
        let path = self.path_of(id)?;
        if attr.size <= self.cfg.content_max {
            let chunk = self.call(self.node.read(&path, 0, None)).await?;
            let out = slice(&chunk.data);
            if chunk.eof && chunk.data.len() as u64 == chunk.size {
                self.store_content(id, chunk.data, chunk.size, chunk.mtime_ms.unwrap_or(0));
                // The read is the freshest view of size and mtime.
                if chunk.size != attr.size || chunk.mtime_ms.unwrap_or(0) != attr.mtime_ms {
                    self.caches.lock().unwrap().attrs.remove(&id);
                }
            }
            return Ok(out);
        }
        let chunk = self.call(self.node.read(&path, offset, Some(count as u64))).await?;
        let eof = chunk.eof || offset + chunk.data.len() as u64 >= chunk.size;
        Ok((chunk.data, eof))
    }

    fn store_content(&self, id: u64, data: Vec<u8>, size: u64, mtime_ms: u64) {
        let mut c = self.caches.lock().unwrap();
        let n = data.len() as u64;
        if n > self.cfg.content_budget {
            return;
        }
        if let Some(old) = c.contents.remove(&id) {
            c.content_bytes -= old.data.len() as u64;
        }
        // Simple budget: drop arbitrary entries until it fits.
        while c.content_bytes + n > self.cfg.content_budget {
            let Some(victim) = c.contents.keys().next().copied() else { break };
            let v = c.contents.remove(&victim).expect("key exists");
            c.content_bytes -= v.data.len() as u64;
        }
        c.content_bytes += n;
        c.contents.insert(id, Content { data: Arc::new(data), size, mtime_ms });
    }

    /// Write in place; returns the new attributes.
    pub async fn write(&self, id: u64, offset: u64, data: &[u8]) -> FsResult<Attr> {
        let path = self.path_of(id)?;
        let s = self.call(self.node.pwrite(&path, offset, data)).await?;
        {
            let mut c = self.caches.lock().unwrap();
            if let Some(old) = c.contents.remove(&id) {
                c.content_bytes -= old.data.len() as u64;
            }
        }
        let a = Attr::of(id, &s);
        self.put_attr(a);
        Ok(a)
    }

    /// Create a regular file. `exclusive` fails with `Exist` if the name exists; otherwise an
    /// existing file is reused (O_CREAT without O_EXCL) and `set` is applied either way.
    pub async fn create(&self, dir: u64, name: &OsStr, set: SetAttr, exclusive: bool) -> FsResult<Attr> {
        let path = self.child_path(dir, name)?;
        match self.call(self.node.create_new(&path)).await {
            Ok(()) => {}
            Err(FsErr::Exist) if !exclusive => {}
            Err(e) => return Err(e),
        }
        self.dir_changed(dir);
        let s = if set.is_empty() {
            self.call(self.node.lstat(&path)).await?
        } else {
            self.call(self.node.setattr(&setattr_req(&path, set))).await?
        };
        Ok(self.remember(&path, &s))
    }

    pub async fn mkdir(&self, dir: u64, name: &OsStr, perm: Option<u32>) -> FsResult<Attr> {
        let path = self.child_path(dir, name)?;
        let s = self.call(self.node.mkdir(&path, perm)).await?;
        self.dir_changed(dir);
        Ok(self.remember(&path, &s))
    }

    /// Remove a file, link or empty directory.
    pub async fn remove(&self, dir: u64, name: &OsStr) -> FsResult<()> {
        let path = self.child_path(dir, name)?;
        self.call(self.node.remove(&path)).await?;
        self.dir_changed(dir);
        let gone = self.inodes.lock().unwrap().forget_tree(&path);
        let mut c = self.caches.lock().unwrap();
        for id in gone {
            c.drop_id(id);
        }
        Ok(())
    }

    pub async fn rename(&self, from_dir: u64, from: &OsStr, to_dir: u64, to: &OsStr) -> FsResult<()> {
        let from_path = self.child_path(from_dir, from)?;
        let to_path = self.child_path(to_dir, to)?;
        if to_path.starts_with(&from_path) && to_path != from_path {
            return Err(FsErr::Inval);
        }
        self.call(self.node.rename(&from_path, &to_path, true)).await?;
        self.dir_changed(from_dir);
        self.dir_changed(to_dir);
        let moved_id = {
            let mut i = self.inodes.lock().unwrap();
            let replaced = if from_path == to_path { Vec::new() } else { i.forget_tree(&to_path) };
            let mut c = self.caches.lock().unwrap();
            for id in replaced {
                c.drop_id(id);
            }
            drop(c);
            i.rename_tree(&from_path, &to_path);
            i.by_path.get(&to_path).copied()
        };
        if let Some(id) = moved_id {
            // ctime changed; a moved directory's listing paths are rebuilt on next readdir.
            self.caches.lock().unwrap().drop_id(id);
        }
        Ok(())
    }

    pub async fn symlink(&self, dir: u64, name: &OsStr, target: &Path) -> FsResult<Attr> {
        let path = self.child_path(dir, name)?;
        let s = self.call(self.node.symlink(&path, target)).await?;
        self.dir_changed(dir);
        Ok(self.remember(&path, &s))
    }

    pub async fn readlink(&self, id: u64) -> FsResult<PathBuf> {
        let path = self.path_of(id)?;
        self.call(self.node.readlink(&path)).await
    }

    pub async fn setattr(&self, id: u64, set: SetAttr) -> FsResult<Attr> {
        if set.is_empty() {
            return self.getattr(id).await;
        }
        let path = self.path_of(id)?;
        let s = self.call(self.node.setattr(&setattr_req(&path, set))).await?;
        if set.size.is_some() {
            let mut c = self.caches.lock().unwrap();
            if let Some(old) = c.contents.remove(&id) {
                c.content_bytes -= old.data.len() as u64;
            }
        }
        let a = Attr::of(id, &s);
        self.put_attr(a);
        Ok(a)
    }
}

fn setattr_req(path: &Path, set: SetAttr) -> SetAttrRequest {
    SetAttrRequest { path: path.into(), mode: set.perm, size: set.size, atime: set.atime, mtime: set.mtime }
}

// ---------------------------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Debug, Clone)]
    enum Node {
        File(Vec<u8>, u32, u64),
        Dir(u32, u64),
        Link(PathBuf),
    }

    /// An in-memory node: paths → entries, a clock for mtimes, call counting, and switches to
    /// simulate an outage or a hang.
    #[derive(Default)]
    pub(crate) struct FakeNode {
        tree: Mutex<BTreeMap<PathBuf, Node>>,
        clock: AtomicUsize,
        pub calls: AtomicUsize,
        pub down: AtomicBool,
        pub hang: AtomicBool,
    }

    impl FakeNode {
        pub(crate) fn new(root: &str) -> Arc<FakeNode> {
            let n = FakeNode::default();
            n.tree.lock().unwrap().insert(root.into(), Node::Dir(0o755, 1));
            Arc::new(n)
        }

        fn tick(&self) -> u64 {
            1_000 + self.clock.fetch_add(1, Ordering::SeqCst) as u64 * 1_000
        }

        pub(crate) fn put_file(&self, path: &str, data: &[u8]) {
            let t = self.tick();
            self.tree.lock().unwrap().insert(path.into(), Node::File(data.to_vec(), 0o644, t));
        }

        pub(crate) fn put_dir(&self, path: &str) {
            let t = self.tick();
            self.tree.lock().unwrap().insert(path.into(), Node::Dir(0o755, t));
        }

        pub(crate) fn get(&self, path: &str) -> Option<Vec<u8>> {
            match self.tree.lock().unwrap().get(Path::new(path)) {
                Some(Node::File(d, ..)) => Some(d.clone()),
                _ => None,
            }
        }

        pub(crate) fn exists(&self, path: &str) -> bool {
            self.tree.lock().unwrap().contains_key(Path::new(path))
        }

        async fn enter(&self) -> FsResult<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.hang.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            if self.down.load(Ordering::SeqCst) {
                return Err(FsErr::Unreachable("connection refused".into()));
            }
            Ok(())
        }

        fn stat(path: &Path, n: &Node) -> Stat {
            let (kind, size, mode, mtime) = match n {
                Node::File(d, m, t) => (FileKind::File, d.len() as u64, *m, *t),
                Node::Dir(m, t) => (FileKind::Dir, 0, *m, *t),
                Node::Link(target) => (FileKind::Symlink, target.as_os_str().len() as u64, 0o777, 0),
            };
            Stat {
                path: path.into(),
                kind,
                size,
                mtime_ms: Some(mtime),
                mode,
                readonly: false,
                ino: None,
                nlink: Some(1),
                atime_ms: None,
                ctime_ms: None,
            }
        }

        fn parent_is_dir(tree: &BTreeMap<PathBuf, Node>, path: &Path) -> FsResult<()> {
            match path.parent().and_then(|p| tree.get(p)) {
                Some(Node::Dir(..)) => Ok(()),
                Some(_) => Err(FsErr::NotDir),
                None => Err(FsErr::NoEnt),
            }
        }
    }

    #[async_trait]
    impl NodeFs for FakeNode {
        async fn lstat(&self, path: &Path) -> FsResult<Stat> {
            self.enter().await?;
            let t = self.tree.lock().unwrap();
            t.get(path).map(|n| FakeNode::stat(path, n)).ok_or(FsErr::NoEnt)
        }

        async fn list(&self, path: &Path) -> FsResult<Vec<ember_node::proto::DirEntry>> {
            self.enter().await?;
            let t = self.tree.lock().unwrap();
            match t.get(path) {
                Some(Node::Dir(..)) => {}
                Some(_) => return Err(FsErr::NotDir),
                None => return Err(FsErr::NoEnt),
            }
            Ok(t.iter()
                .filter(|(p, _)| p.parent() == Some(path))
                .map(|(p, n)| {
                    let s = FakeNode::stat(p, n);
                    ember_node::proto::DirEntry {
                        name: p.file_name().unwrap().to_string_lossy().into_owned(),
                        kind: s.kind,
                        size: s.size,
                        mtime_ms: s.mtime_ms,
                        mode: Some(s.mode),
                        ino: None,
                    }
                })
                .collect())
        }

        async fn read(&self, path: &Path, offset: u64, len: Option<u64>) -> FsResult<ReadChunk> {
            self.enter().await?;
            let t = self.tree.lock().unwrap();
            match t.get(path) {
                Some(Node::File(d, _, mtime)) => {
                    let start = (offset as usize).min(d.len());
                    let end = len.map(|l| (start + l as usize).min(d.len())).unwrap_or(d.len());
                    Ok(ReadChunk {
                        data: d[start..end].to_vec(),
                        size: d.len() as u64,
                        mtime_ms: Some(*mtime),
                        eof: end >= d.len(),
                    })
                }
                Some(Node::Dir(..)) => Err(FsErr::IsDir),
                Some(_) => Err(FsErr::Inval),
                None => Err(FsErr::NoEnt),
            }
        }

        async fn pwrite(&self, path: &Path, offset: u64, data: &[u8]) -> FsResult<Stat> {
            self.enter().await?;
            let now = self.tick();
            let mut t = self.tree.lock().unwrap();
            match t.get_mut(path) {
                Some(Node::File(d, _, mtime)) => {
                    let end = offset as usize + data.len();
                    if d.len() < end {
                        d.resize(end, 0);
                    }
                    d[offset as usize..end].copy_from_slice(data);
                    *mtime = now;
                }
                Some(_) => return Err(FsErr::Inval),
                None => return Err(FsErr::NoEnt),
            }
            Ok(FakeNode::stat(path, t.get(path).unwrap()))
        }

        async fn create_new(&self, path: &Path) -> FsResult<()> {
            self.enter().await?;
            let now = self.tick();
            let mut t = self.tree.lock().unwrap();
            FakeNode::parent_is_dir(&t, path)?;
            if t.contains_key(path) {
                return Err(FsErr::Exist);
            }
            t.insert(path.into(), Node::File(Vec::new(), 0o644, now));
            Ok(())
        }

        async fn mkdir(&self, path: &Path, mode: Option<u32>) -> FsResult<Stat> {
            self.enter().await?;
            let now = self.tick();
            let mut t = self.tree.lock().unwrap();
            FakeNode::parent_is_dir(&t, path)?;
            if t.contains_key(path) {
                return Err(FsErr::Exist);
            }
            t.insert(path.into(), Node::Dir(mode.unwrap_or(0o755), now));
            Ok(FakeNode::stat(path, t.get(path).unwrap()))
        }

        async fn remove(&self, path: &Path) -> FsResult<()> {
            self.enter().await?;
            let mut t = self.tree.lock().unwrap();
            match t.get(path) {
                None => return Err(FsErr::NoEnt),
                Some(Node::Dir(..)) if t.keys().any(|p| p.parent() == Some(path)) => return Err(FsErr::NotEmpty),
                Some(_) => {}
            }
            t.remove(path);
            Ok(())
        }

        async fn rename(&self, from: &Path, to: &Path, overwrite: bool) -> FsResult<()> {
            self.enter().await?;
            let mut t = self.tree.lock().unwrap();
            if !t.contains_key(from) {
                return Err(FsErr::NoEnt);
            }
            FakeNode::parent_is_dir(&t, to)?;
            if t.contains_key(to) && !overwrite {
                return Err(FsErr::Exist);
            }
            let moved: Vec<PathBuf> = t.keys().filter(|p| p.starts_with(from)).cloned().collect();
            let old_target: Vec<PathBuf> = t.keys().filter(|p| p.starts_with(to)).cloned().collect();
            for p in old_target {
                t.remove(&p);
            }
            for p in moved {
                let n = t.remove(&p).unwrap();
                let rest = p.strip_prefix(from).unwrap();
                let np = if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) };
                t.insert(np, n);
            }
            Ok(())
        }

        async fn symlink(&self, path: &Path, target: &Path) -> FsResult<Stat> {
            self.enter().await?;
            let mut t = self.tree.lock().unwrap();
            FakeNode::parent_is_dir(&t, path)?;
            if t.contains_key(path) {
                return Err(FsErr::Exist);
            }
            t.insert(path.into(), Node::Link(target.into()));
            Ok(FakeNode::stat(path, t.get(path).unwrap()))
        }

        async fn readlink(&self, path: &Path) -> FsResult<PathBuf> {
            self.enter().await?;
            match self.tree.lock().unwrap().get(path) {
                Some(Node::Link(target)) => Ok(target.clone()),
                Some(_) => Err(FsErr::Inval),
                None => Err(FsErr::NoEnt),
            }
        }

        async fn setattr(&self, req: &SetAttrRequest) -> FsResult<Stat> {
            self.enter().await?;
            let now = self.tick();
            let mut t = self.tree.lock().unwrap();
            let n = t.get_mut(&req.path).ok_or(FsErr::NoEnt)?;
            match n {
                Node::File(d, m, mtime) => {
                    if let Some(size) = req.size {
                        d.resize(size as usize, 0);
                        *mtime = now;
                    }
                    if let Some(mode) = req.mode {
                        *m = mode;
                    }
                    match req.mtime {
                        Some(SetTime::UnixMs(ms)) => *mtime = ms,
                        Some(SetTime::Now) => *mtime = now,
                        None => {}
                    }
                }
                Node::Dir(m, _) => {
                    if req.size.is_some() {
                        return Err(FsErr::IsDir);
                    }
                    if let Some(mode) = req.mode {
                        *m = mode;
                    }
                }
                Node::Link(_) => {}
            }
            Ok(FakeNode::stat(&req.path, t.get(&req.path).unwrap()))
        }
    }

    const ROOT: &str = "/home/pi/proj";

    fn fs_with(node: &Arc<FakeNode>, ttl: Duration) -> RemoteFs {
        let cfg = CacheConfig { attr_ttl: ttl, op_timeout: Duration::from_millis(200), ..CacheConfig::default() };
        RemoteFs::new(node.clone(), ROOT.into(), cfg)
    }

    fn os(s: &str) -> &OsStr {
        OsStr::new(s)
    }

    #[tokio::test]
    async fn lookup_read_and_listing_use_the_cache() {
        let node = FakeNode::new(ROOT);
        node.put_file("/home/pi/proj/a.txt", b"hello");
        let fs = fs_with(&node, Duration::from_secs(60));

        let entries = fs.readdir(ROOT_ID).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "a.txt");
        let calls = node.calls.load(Ordering::SeqCst);
        // Lookup and getattr after a listing are answered locally.
        let a = fs.lookup(ROOT_ID, os("a.txt")).await.unwrap();
        assert_eq!(a.size, 5);
        assert_eq!(fs.lookup(ROOT_ID, os("missing")).await.unwrap_err(), FsErr::NoEnt);
        assert_eq!(node.calls.load(Ordering::SeqCst), calls);

        // The first read fetches the whole small file; later ranges come from memory.
        assert_eq!(fs.read(a.id, 1, 3).await.unwrap(), (b"ell".to_vec(), false));
        let calls = node.calls.load(Ordering::SeqCst);
        assert_eq!(fs.read(a.id, 3, 100).await.unwrap(), (b"lo".to_vec(), true));
        assert_eq!(fs.read(a.id, 10, 4).await.unwrap(), (Vec::new(), true));
        assert_eq!(node.calls.load(Ordering::SeqCst), calls);
        assert_eq!(fs.path_of(a.id).unwrap(), Path::new("/home/pi/proj/a.txt"));
    }

    #[tokio::test]
    async fn changes_on_the_node_show_after_the_ttl_or_an_invalidation() {
        let node = FakeNode::new(ROOT);
        node.put_file("/home/pi/proj/a.txt", b"one");
        let fs = fs_with(&node, Duration::from_secs(60));
        let a = fs.lookup(ROOT_ID, os("a.txt")).await.unwrap();
        assert_eq!(fs.read(a.id, 0, 100).await.unwrap().0, b"one");

        // Changed behind the mount's back (e.g. by the agent's Bash tool on the node).
        node.put_file("/home/pi/proj/a.txt", b"two!");
        node.put_file("/home/pi/proj/b.txt", b"new");
        assert_eq!(fs.read(a.id, 0, 100).await.unwrap().0, b"one", "still within the TTL");
        fs.invalidate_all();
        assert_eq!(fs.read(a.id, 0, 100).await.unwrap().0, b"two!");
        let names: Vec<_> = fs.readdir(ROOT_ID).await.unwrap().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["a.txt", "b.txt"]);

        // With a zero TTL every call goes to the node.
        let fs = fs_with(&node, Duration::ZERO);
        let a = fs.lookup(ROOT_ID, os("a.txt")).await.unwrap();
        node.put_file("/home/pi/proj/a.txt", b"three");
        assert_eq!(fs.read(a.id, 0, 100).await.unwrap().0, b"three");
    }

    #[tokio::test]
    async fn create_write_truncate_and_setattr() {
        let node = FakeNode::new(ROOT);
        let fs = fs_with(&node, Duration::from_secs(60));
        let f = fs.create(ROOT_ID, os("new.rs"), SetAttr { perm: Some(0o600), ..Default::default() }, true).await.unwrap();
        assert_eq!(f.kind, FileKind::File);
        assert_eq!(f.perm, 0o600);
        assert_eq!(fs.create(ROOT_ID, os("new.rs"), SetAttr::default(), true).await.unwrap_err(), FsErr::Exist);

        let a = fs.write(f.id, 0, b"fn main() {}").await.unwrap();
        assert_eq!(a.size, 12);
        assert_eq!(fs.read(f.id, 0, 100).await.unwrap().0, b"fn main() {}");
        let a = fs.write(f.id, 3, b"MAIN").await.unwrap();
        assert_eq!(a.size, 12);
        assert_eq!(fs.read(f.id, 0, 100).await.unwrap().0, b"fn MAIN() {}", "a write drops cached contents");
        assert_eq!(node.get("/home/pi/proj/new.rs").unwrap(), b"fn MAIN() {}");

        // O_CREAT|O_TRUNC on an existing file: non-exclusive create with size 0.
        let t = fs.create(ROOT_ID, os("new.rs"), SetAttr { size: Some(0), ..Default::default() }, false).await.unwrap();
        assert_eq!(t.id, f.id);
        assert_eq!(t.size, 0);
        assert_eq!(fs.read(f.id, 0, 100).await.unwrap(), (Vec::new(), true));

        let a = fs.setattr(f.id, SetAttr { mtime: Some(SetTime::UnixMs(42_000)), ..Default::default() }).await.unwrap();
        assert_eq!(a.mtime_ms, 42_000);
        // The new file shows up in the parent's listing (the listing was invalidated).
        let names: Vec<_> = fs.readdir(ROOT_ID).await.unwrap().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["new.rs"]);
    }

    #[tokio::test]
    async fn mkdir_rename_keeps_ids_and_remove_forgets_them() {
        let node = FakeNode::new(ROOT);
        let fs = fs_with(&node, Duration::from_secs(60));
        let d = fs.mkdir(ROOT_ID, os("src"), Some(0o750)).await.unwrap();
        assert_eq!(d.kind, FileKind::Dir);
        let f = fs.create(d.id, os("lib.rs"), SetAttr::default(), true).await.unwrap();
        fs.write(f.id, 0, b"x").await.unwrap();

        // Editors save by writing a temp file and renaming it over the original.
        let tmp = fs.create(d.id, os(".lib.rs.swp"), SetAttr::default(), true).await.unwrap();
        fs.write(tmp.id, 0, b"y").await.unwrap();
        fs.rename(d.id, os(".lib.rs.swp"), d.id, os("lib.rs")).await.unwrap();
        assert_eq!(node.get("/home/pi/proj/src/lib.rs").unwrap(), b"y");
        let now = fs.lookup(d.id, os("lib.rs")).await.unwrap();
        assert_eq!(now.id, tmp.id, "the renamed file keeps its id");
        assert_eq!(fs.path_of(f.id).unwrap_err(), FsErr::Stale, "the replaced file is gone");
        assert_eq!(fs.read(now.id, 0, 10).await.unwrap().0, b"y");

        // Moving a directory moves everything under it.
        fs.rename(ROOT_ID, os("src"), ROOT_ID, os("lib")).await.unwrap();
        assert_eq!(fs.path_of(now.id).unwrap(), Path::new("/home/pi/proj/lib/lib.rs"));
        assert_eq!(fs.path_of(d.id).unwrap(), Path::new("/home/pi/proj/lib"));
        assert_eq!(fs.rename(ROOT_ID, os("lib"), d.id, os("inside")).await.unwrap_err(), FsErr::Inval);

        assert_eq!(fs.remove(ROOT_ID, os("lib")).await.unwrap_err(), FsErr::NotEmpty);
        fs.remove(d.id, os("lib.rs")).await.unwrap();
        fs.remove(ROOT_ID, os("lib")).await.unwrap();
        assert!(!node.exists("/home/pi/proj/lib"));
        assert_eq!(fs.path_of(d.id).unwrap_err(), FsErr::Stale);
        assert_eq!(fs.lookup(ROOT_ID, os("lib")).await.unwrap_err(), FsErr::NoEnt);
    }

    #[tokio::test]
    async fn symlinks_are_kept_as_links() {
        let node = FakeNode::new(ROOT);
        let fs = fs_with(&node, Duration::from_secs(60));
        let l = fs.symlink(ROOT_ID, os("cfg"), Path::new("../shared/cfg.toml")).await.unwrap();
        assert_eq!(l.kind, FileKind::Symlink);
        assert_eq!(fs.readlink(l.id).await.unwrap(), Path::new("../shared/cfg.toml"));
        assert_eq!(fs.read(l.id, 0, 10).await.unwrap_err(), FsErr::Inval);
    }

    #[tokio::test]
    async fn names_are_checked_before_reaching_the_node() {
        let node = FakeNode::new(ROOT);
        let fs = fs_with(&node, Duration::from_secs(60));
        for bad in ["", "a/b", "..", "."] {
            assert_eq!(fs.create(ROOT_ID, os(bad), SetAttr::default(), true).await.unwrap_err(), FsErr::Inval, "{bad:?}");
        }
        let long = "x".repeat(256);
        assert_eq!(fs.mkdir(ROOT_ID, os(&long), None).await.unwrap_err(), FsErr::NameTooLong);
        assert_eq!(fs.getattr(999).await.unwrap_err(), FsErr::Stale);
        assert_eq!(node.calls.load(Ordering::SeqCst), 0);
        // `.` and `..` resolve locally; the root is its own parent.
        assert_eq!(fs.lookup(ROOT_ID, os("..")).await.unwrap().id, ROOT_ID);
    }

    #[tokio::test]
    async fn an_unreachable_or_hung_node_fails_fast_and_is_reported() {
        let node = FakeNode::new(ROOT);
        node.put_file("/home/pi/proj/a.txt", b"a");
        let fs = fs_with(&node, Duration::ZERO);
        let a = fs.lookup(ROOT_ID, os("a.txt")).await.unwrap();
        assert!(fs.health().is_none());

        node.down.store(true, Ordering::SeqCst);
        let e = fs.getattr(a.id).await.unwrap_err();
        assert!(matches!(e, FsErr::Unreachable(_)), "{e:?}");
        assert_eq!(e.errno(), libc::EIO);
        let outage = fs.health().expect("outage recorded");
        assert!(outage.error.contains("unreachable"), "{outage:?}");

        node.down.store(false, Ordering::SeqCst);
        node.hang.store(true, Ordering::SeqCst);
        let started = Instant::now();
        let e = fs.read(a.id, 0, 1).await.unwrap_err();
        assert_eq!(e, FsErr::TimedOut);
        assert_eq!(e.errno(), libc::ETIMEDOUT);
        assert!(started.elapsed() < Duration::from_secs(5), "the deadline bounds the wait");

        node.hang.store(false, Ordering::SeqCst);
        fs.getattr(a.id).await.unwrap();
        assert!(fs.health().is_none(), "a successful call clears the outage");
    }

    #[tokio::test]
    async fn a_file_deleted_on_the_node_becomes_stale() {
        let node = FakeNode::new(ROOT);
        node.put_file("/home/pi/proj/a.txt", b"a");
        let fs = fs_with(&node, Duration::ZERO);
        let a = fs.lookup(ROOT_ID, os("a.txt")).await.unwrap();
        node.tree.lock().unwrap().remove(Path::new("/home/pi/proj/a.txt"));
        assert_eq!(fs.getattr(a.id).await.unwrap_err(), FsErr::NoEnt);
        assert_eq!(fs.getattr(a.id).await.unwrap_err(), FsErr::Stale);
    }

    #[test]
    fn node_errors_map_to_errnos() {
        use ember_node::proto::ErrorBody;
        let api = |status, code, errno: Option<&str>| ClientError::Api {
            status,
            body: ErrorBody { code, error: "x".into(), actual_sha256: None, errno: errno.map(str::to_string) },
        };
        assert_eq!(FsErr::from(api(409, ErrorCode::BadRequest, Some("ENOTEMPTY"))), FsErr::NotEmpty);
        assert_eq!(FsErr::from(api(404, ErrorCode::NotFound, None)), FsErr::NoEnt);
        assert_eq!(FsErr::from(api(403, ErrorCode::ForbiddenPath, None)), FsErr::Access);
        assert_eq!(FsErr::from(api(412, ErrorCode::PreconditionFailed, None)), FsErr::Exist);
        assert!(matches!(FsErr::from(api(500, ErrorCode::Internal, Some("EWEIRD"))), FsErr::Io(_)));
        assert!(matches!(FsErr::from(ClientError::Protocol("p".into())), FsErr::Io(_)));
        // A transport-backed client: dial failures are an outage, a missed deadline a timeout.
        assert!(matches!(FsErr::from(ClientError::Transport("no route".into())), FsErr::Unreachable(_)));
        assert_eq!(FsErr::from(ClientError::Timeout(Duration::from_secs(4))), FsErr::TimedOut);
    }
}
