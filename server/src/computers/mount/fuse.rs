//! FUSE front end for [`RemoteFs`] (feature `mount-fuse`, Linux only), via the `fuser` crate.
//! Mounting as a normal user goes through the setuid `fusermount3` helper (Debian / Raspberry
//! Pi OS package `fuse3`); no root needed when the mount point is owned by this user.
//!
//! Each request is answered from a task on the server's tokio runtime, so a slow node call
//! blocks neither the FUSE session thread nor other requests. [`RemoteFs`] bounds every node
//! call, so every request gets an answer (`EIO` / `ETIMEDOUT` at worst) and no process is left
//! in uninterruptible sleep; if this process dies the kernel fails requests with `ENOTCONN`.
//!
//! Written against `fuser` 0.18.0 (`Filesystem` with `&self` methods and `INodeNo` /
//! `FileHandle` newtypes), read from the published crate source; **not compiled**.

use std::ffi::OsStr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ember_node::proto::{FileKind, SetTime};
use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, LockOwner, OpenFlags,
    RenameFlags, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite,
    Request, TimeOrNow, WriteFlags,
};
use tokio::runtime::Handle;

use super::remote_fs::{Attr, FsErr, RemoteFs, SetAttr};

/// [`RemoteFs`] as a `fuser` filesystem.
pub struct FuseAdapter {
    fs: Arc<RemoteFs>,
    rt: Handle,
    /// Kernel cache lifetime for entries and attributes (same as the mount's attribute TTL).
    ttl: Duration,
    uid: u32,
    gid: u32,
}

impl FuseAdapter {
    pub fn new(fs: Arc<RemoteFs>, rt: Handle, ttl: Duration) -> FuseAdapter {
        // SAFETY: getuid/getgid cannot fail.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        FuseAdapter { fs, rt, ttl, uid, gid }
    }
}

fn errno(e: FsErr) -> Errno {
    Errno::from_i32(e.errno())
}

fn file_type(k: FileKind) -> FileType {
    match k {
        FileKind::Dir => FileType::Directory,
        FileKind::Symlink => FileType::Symlink,
        FileKind::File | FileKind::Other => FileType::RegularFile,
    }
}

fn file_attr(a: &Attr, uid: u32, gid: u32) -> FileAttr {
    FileAttr {
        ino: INodeNo(a.id),
        size: a.size,
        blocks: a.size.div_ceil(512),
        atime: Attr::time(a.atime_ms),
        mtime: Attr::time(a.mtime_ms),
        ctime: Attr::time(a.ctime_ms),
        crtime: Attr::time(a.ctime_ms),
        kind: file_type(a.kind),
        perm: (a.perm & 0o7777) as u16,
        nlink: a.nlink.max(1),
        uid,
        gid,
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}

fn set_time(t: Option<TimeOrNow>) -> Option<SetTime> {
    match t? {
        TimeOrNow::Now => Some(SetTime::Now),
        TimeOrNow::SpecificTime(st) => {
            let ms = st.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
            Some(SetTime::UnixMs(ms))
        }
    }
}

impl Filesystem for FuseAdapter {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let (fs, ttl, uid, gid, name) = (self.fs.clone(), self.ttl, self.uid, self.gid, name.to_owned());
        self.rt.spawn(async move {
            match fs.lookup(parent.0, &name).await {
                Ok(a) => reply.entry(&ttl, &file_attr(&a, uid, gid), Generation(0)),
                Err(e) => reply.error(errno(e)),
            }
        });
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let (fs, ttl, uid, gid) = (self.fs.clone(), self.ttl, self.uid, self.gid);
        self.rt.spawn(async move {
            match fs.getattr(ino.0).await {
                Ok(a) => reply.attr(&ttl, &file_attr(&a, uid, gid)),
                Err(e) => reply.error(errno(e)),
            }
        });
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        // Ownership changes are ignored (everything is shown as this user's; the node cannot chown).
        let set = SetAttr { perm: mode.map(|m| m & 0o7777), size, atime: set_time(atime), mtime: set_time(mtime) };
        let (fs, ttl, uid, gid) = (self.fs.clone(), self.ttl, self.uid, self.gid);
        self.rt.spawn(async move {
            match fs.setattr(ino.0, set).await {
                Ok(a) => reply.attr(&ttl, &file_attr(&a, uid, gid)),
                Err(e) => reply.error(errno(e)),
            }
        });
    }

    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData) {
        let fs = self.fs.clone();
        self.rt.spawn(async move {
            use std::os::unix::ffi::OsStrExt;
            match fs.readlink(ino.0).await {
                Ok(t) => reply.data(t.as_os_str().as_bytes()),
                Err(e) => reply.error(errno(e)),
            }
        });
    }

    fn mkdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, mode: u32, umask: u32, reply: ReplyEntry) {
        let (fs, ttl, uid, gid, name) = (self.fs.clone(), self.ttl, self.uid, self.gid, name.to_owned());
        self.rt.spawn(async move {
            match fs.mkdir(parent.0, &name, Some(mode & !umask & 0o7777)).await {
                Ok(a) => reply.entry(&ttl, &file_attr(&a, uid, gid), Generation(0)),
                Err(e) => reply.error(errno(e)),
            }
        });
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        self.remove(parent, name, reply);
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        self.remove(parent, name, reply);
    }

    fn symlink(&self, _req: &Request, parent: INodeNo, link_name: &OsStr, target: &Path, reply: ReplyEntry) {
        let (fs, ttl, uid, gid) = (self.fs.clone(), self.ttl, self.uid, self.gid);
        let (name, target) = (link_name.to_owned(), target.to_owned());
        self.rt.spawn(async move {
            match fs.symlink(parent.0, &name, &target).await {
                Ok(a) => reply.entry(&ttl, &file_attr(&a, uid, gid), Generation(0)),
                Err(e) => reply.error(errno(e)),
            }
        });
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        if !flags.is_empty() {
            // RENAME_NOREPLACE / RENAME_EXCHANGE are not supported by the node API; callers
            // (e.g. `mv -n`) fall back to a plain rename after EINVAL.
            reply.error(Errno::EINVAL);
            return;
        }
        let (fs, name, newname) = (self.fs.clone(), name.to_owned(), newname.to_owned());
        self.rt.spawn(async move {
            match fs.rename(parent.0, &name, newparent.0, &newname).await {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(errno(e)),
            }
        });
    }

    fn open(&self, _req: &Request, _ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        // Stateless: reads and writes go by inode. Do not keep the kernel page cache across
        // opens, so a file changed on the node is re-read (close-to-open, like NFS).
        reply.opened(FileHandle(0), FopenFlags::empty());
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let fs = self.fs.clone();
        self.rt.spawn(async move {
            match fs.read(ino.0, offset, size).await {
                Ok((data, _eof)) => reply.data(&data),
                Err(e) => reply.error(errno(e)),
            }
        });
    }

    fn write(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        let (fs, data) = (self.fs.clone(), data.to_vec());
        self.rt.spawn(async move {
            match fs.write(ino.0, offset, &data).await {
                Ok(_) => reply.written(data.len() as u32),
                Err(e) => reply.error(errno(e)),
            }
        });
    }

    fn flush(&self, _req: &Request, _ino: INodeNo, _fh: FileHandle, _lock_owner: LockOwner, reply: ReplyEmpty) {
        // Writes are already on the node.
        reply.ok();
    }

    fn fsync(&self, _req: &Request, _ino: INodeNo, _fh: FileHandle, _datasync: bool, reply: ReplyEmpty) {
        reply.ok();
    }

    fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
        let fs = self.fs.clone();
        self.rt.spawn(async move {
            match fs.readdir(ino.0).await {
                Ok(entries) => {
                    // Offsets: 1 = ".", 2 = "..", 3.. = entries; `offset` is the last one seen.
                    let mut all = vec![(OsStr::new(".").to_owned(), ino.0, FileType::Directory)];
                    all.push((OsStr::new("..").to_owned(), ino.0, FileType::Directory));
                    all.extend(entries.into_iter().map(|(n, a)| (n, a.id, file_type(a.kind))));
                    for (i, (name, id, kind)) in all.into_iter().enumerate().skip(offset as usize) {
                        if reply.add(INodeNo(id), i as u64 + 1, kind, &name) {
                            break;
                        }
                    }
                    reply.ok();
                }
                Err(e) => reply.error(errno(e)),
            }
        });
    }

    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let exclusive = flags & libc::O_EXCL != 0;
        let mut set = SetAttr { perm: Some(mode & !umask & 0o7777), ..SetAttr::default() };
        if flags & libc::O_TRUNC != 0 {
            set.size = Some(0);
        }
        let (fs, ttl, uid, gid, name) = (self.fs.clone(), self.ttl, self.uid, self.gid, name.to_owned());
        self.rt.spawn(async move {
            match fs.create(parent.0, &name, set, exclusive).await {
                Ok(a) => reply.created(&ttl, &file_attr(&a, uid, gid), Generation(0), FileHandle(0), FopenFlags::empty()),
                Err(e) => reply.error(errno(e)),
            }
        });
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: fuser::ReplyStatfs) {
        // Unknown capacity: report plenty so tools do not refuse to write.
        reply.statfs(1 << 30, 1 << 29, 1 << 29, 1 << 20, 1 << 19, 4096, 255, 4096);
    }
}

impl FuseAdapter {
    fn remove(&self, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let (fs, name) = (self.fs.clone(), name.to_owned());
        self.rt.spawn(async move {
            match fs.remove(parent.0, &name).await {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(errno(e)),
            }
        });
    }
}

/// A mounted FUSE session; unmounts when [`FuseMount::unmount`] is called or it is dropped.
pub struct FuseMount {
    session: Option<fuser::BackgroundSession>,
}

impl FuseMount {
    /// Mount `fs` at `at` (an existing directory owned by this user).
    pub fn mount(fs: Arc<RemoteFs>, at: &Path, ttl: Duration, rt: Handle) -> std::io::Result<FuseMount> {
        let mut cfg = fuser::Config::default();
        cfg.mount_options = vec![
            fuser::MountOption::FSName(format!("ember:{}", fs.root().display())),
            fuser::MountOption::Subtype("ember".into()),
            fuser::MountOption::NoDev,
            fuser::MountOption::NoSuid,
            fuser::MountOption::DefaultPermissions,
        ];
        cfg.n_threads = Some(2);
        let session = fuser::spawn_mount(FuseAdapter::new(fs, rt, ttl), at, &cfg)?;
        Ok(FuseMount { session: Some(session) })
    }

    pub fn unmount(mut self) -> std::io::Result<()> {
        match self.session.take() {
            Some(s) => s.umount_and_join(),
            None => Ok(()),
        }
    }
}
