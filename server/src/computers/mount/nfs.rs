//! NFSv3 front end for [`RemoteFs`] (feature `mount-nfs`), served on `127.0.0.1:<ephemeral>` by
//! the `nfsserve` crate and mounted with the OS's own NFS client (`mount_nfs` on macOS — no
//! kernel extension; see [`super::cmd`]).
//!
//! Written against `nfsserve` 0.11.0 (`vfs::NFSFileSystem`, `tcp::NFSTcpListener`), read from
//! the published crate source; **not compiled**.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use ember_node::proto::{FileKind, SetTime};
use nfsserve::nfs::{
    fattr3, fileid3, filename3, ftype3, nfspath3, nfsstat3, nfsstring, nfstime3, sattr3, set_atime, set_mode3,
    set_mtime, set_size3, specdata3,
};
use nfsserve::tcp::{NFSTcp, NFSTcpListener};
use nfsserve::vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};

use super::remote_fs::{Attr, FsErr, RemoteFs, SetAttr, ROOT_ID};

/// [`RemoteFs`] as an `nfsserve` filesystem.
pub struct NfsAdapter {
    fs: Arc<RemoteFs>,
    uid: u32,
    gid: u32,
    fsid: u64,
}

impl NfsAdapter {
    pub fn new(fs: Arc<RemoteFs>) -> NfsAdapter {
        // Everything is reported as owned by this process's user, so the kernel's permission
        // checks (on the mode bits) pass for the agent, which runs as the same user.
        // SAFETY: getuid/getgid cannot fail.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        let fsid = fsid_of(fs.root());
        NfsAdapter { fs, uid, gid, fsid }
    }

    fn fattr(&self, a: &Attr) -> fattr3 {
        fattr3 {
            ftype: match a.kind {
                FileKind::Dir => ftype3::NF3DIR,
                FileKind::Symlink => ftype3::NF3LNK,
                // Sockets, FIFOs and devices are shown as files; the node API cannot open them
                // anyway.
                FileKind::File | FileKind::Other => ftype3::NF3REG,
            },
            mode: a.perm,
            nlink: a.nlink.max(1),
            uid: self.uid,
            gid: self.gid,
            size: a.size,
            used: a.size,
            rdev: specdata3::default(),
            fsid: self.fsid,
            fileid: a.id,
            atime: nfstime(a.atime_ms),
            mtime: nfstime(a.mtime_ms),
            ctime: nfstime(a.ctime_ms),
        }
    }
}

/// A stable per-root filesystem id (so two mounts never share one).
fn fsid_of(root: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut h);
    h.finish()
}

fn nfstime(ms: u64) -> nfstime3 {
    nfstime3 { seconds: (ms / 1000).min(u32::MAX as u64) as u32, nseconds: ((ms % 1000) * 1_000_000) as u32 }
}

fn ms_of(t: nfstime3) -> u64 {
    t.seconds as u64 * 1000 + t.nseconds as u64 / 1_000_000
}

fn name(f: &filename3) -> &OsStr {
    OsStr::from_bytes(&f.0)
}

/// NFS has no ETIMEDOUT; an outage is `NFS3ERR_IO` (never `JUKEBOX`, which makes clients retry
/// forever — exactly the hang this mount must avoid).
pub fn nfs_status(e: FsErr) -> nfsstat3 {
    match e {
        FsErr::NoEnt => nfsstat3::NFS3ERR_NOENT,
        FsErr::Exist => nfsstat3::NFS3ERR_EXIST,
        FsErr::NotDir => nfsstat3::NFS3ERR_NOTDIR,
        FsErr::IsDir => nfsstat3::NFS3ERR_ISDIR,
        FsErr::NotEmpty => nfsstat3::NFS3ERR_NOTEMPTY,
        FsErr::Access | FsErr::Busy => nfsstat3::NFS3ERR_ACCES,
        FsErr::Perm => nfsstat3::NFS3ERR_PERM,
        FsErr::Inval | FsErr::Loop => nfsstat3::NFS3ERR_INVAL,
        FsErr::NameTooLong => nfsstat3::NFS3ERR_NAMETOOLONG,
        FsErr::NoSpc => nfsstat3::NFS3ERR_NOSPC,
        FsErr::Rofs => nfsstat3::NFS3ERR_ROFS,
        FsErr::XDev => nfsstat3::NFS3ERR_XDEV,
        FsErr::NotSupp => nfsstat3::NFS3ERR_NOTSUPP,
        FsErr::Stale => nfsstat3::NFS3ERR_STALE,
        FsErr::Io(_) | FsErr::Unreachable(_) | FsErr::TimedOut => nfsstat3::NFS3ERR_IO,
    }
}

/// The parts of `sattr3` the node can apply. Ownership changes are ignored: everything is shown
/// as owned by this user, and the node cannot chown.
pub fn set_of(s: &sattr3) -> SetAttr {
    SetAttr {
        perm: match s.mode {
            set_mode3::mode(m) => Some(m & 0o7777),
            set_mode3::Void => None,
        },
        size: match s.size {
            set_size3::size(n) => Some(n),
            set_size3::Void => None,
        },
        atime: match s.atime {
            set_atime::DONT_CHANGE => None,
            set_atime::SET_TO_SERVER_TIME => Some(SetTime::Now),
            set_atime::SET_TO_CLIENT_TIME(t) => Some(SetTime::UnixMs(ms_of(t))),
        },
        mtime: match s.mtime {
            set_mtime::DONT_CHANGE => None,
            set_mtime::SET_TO_SERVER_TIME => Some(SetTime::Now),
            set_mtime::SET_TO_CLIENT_TIME(t) => Some(SetTime::UnixMs(ms_of(t))),
        },
    }
}

#[async_trait]
impl NFSFileSystem for NfsAdapter {
    fn capabilities(&self) -> VFSCapabilities {
        VFSCapabilities::ReadWrite
    }

    fn root_dir(&self) -> fileid3 {
        ROOT_ID
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        self.fs.lookup(dirid, name(filename)).await.map(|a| a.id).map_err(nfs_status)
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        match self.fs.getattr(id).await {
            Ok(a) => Ok(self.fattr(&a)),
            // A handle whose file vanished on the node is stale, not "no such file".
            Err(FsErr::NoEnt) => Err(nfsstat3::NFS3ERR_STALE),
            Err(e) => Err(nfs_status(e)),
        }
    }

    async fn setattr(&self, id: fileid3, setattr: sattr3) -> Result<fattr3, nfsstat3> {
        self.fs.setattr(id, set_of(&setattr)).await.map(|a| self.fattr(&a)).map_err(nfs_status)
    }

    async fn read(&self, id: fileid3, offset: u64, count: u32) -> Result<(Vec<u8>, bool), nfsstat3> {
        self.fs.read(id, offset, count).await.map_err(nfs_status)
    }

    async fn write(&self, id: fileid3, offset: u64, data: &[u8]) -> Result<fattr3, nfsstat3> {
        self.fs.write(id, offset, data).await.map(|a| self.fattr(&a)).map_err(nfs_status)
    }

    async fn create(&self, dirid: fileid3, filename: &filename3, attr: sattr3) -> Result<(fileid3, fattr3), nfsstat3> {
        // UNCHECKED and GUARDED (nfsserve checks GUARDED itself before calling this).
        let a = self.fs.create(dirid, name(filename), set_of(&attr), false).await.map_err(nfs_status)?;
        Ok((a.id, self.fattr(&a)))
    }

    async fn create_exclusive(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        self.fs.create(dirid, name(filename), SetAttr::default(), true).await.map(|a| a.id).map_err(nfs_status)
    }

    async fn mkdir(&self, dirid: fileid3, dirname: &filename3) -> Result<(fileid3, fattr3), nfsstat3> {
        let a = self.fs.mkdir(dirid, name(dirname), None).await.map_err(nfs_status)?;
        Ok((a.id, self.fattr(&a)))
    }

    async fn remove(&self, dirid: fileid3, filename: &filename3) -> Result<(), nfsstat3> {
        self.fs.remove(dirid, name(filename)).await.map_err(nfs_status)
    }

    async fn rename(
        &self,
        from_dirid: fileid3,
        from_filename: &filename3,
        to_dirid: fileid3,
        to_filename: &filename3,
    ) -> Result<(), nfsstat3> {
        self.fs
            .rename(from_dirid, name(from_filename), to_dirid, name(to_filename))
            .await
            .map_err(nfs_status)
    }

    async fn readdir(&self, dirid: fileid3, start_after: fileid3, max_entries: usize) -> Result<ReadDirResult, nfsstat3> {
        let entries = self.fs.readdir(dirid).await.map_err(nfs_status)?;
        // nfsserve pages by "the id after which to continue"; 0 starts at the beginning.
        let start = if start_after == 0 {
            0
        } else {
            match entries.iter().position(|(_, a)| a.id == start_after) {
                Some(i) => i + 1,
                // The cookie's entry went away between pages: make the client restart.
                None => return Err(nfsstat3::NFS3ERR_BAD_COOKIE),
            }
        };
        let page: Vec<DirEntry> = entries
            .iter()
            .skip(start)
            .take(max_entries)
            .map(|(n, a)| DirEntry { fileid: a.id, name: nfsstring(n.as_bytes().to_vec()), attr: self.fattr(a) })
            .collect();
        let end = start + page.len() >= entries.len();
        Ok(ReadDirResult { entries: page, end })
    }

    async fn symlink(
        &self,
        dirid: fileid3,
        linkname: &filename3,
        symlink: &nfspath3,
        _attr: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        let target = Path::new(OsStr::from_bytes(&symlink.0));
        let a = self.fs.symlink(dirid, name(linkname), target).await.map_err(nfs_status)?;
        Ok((a.id, self.fattr(&a)))
    }

    async fn readlink(&self, id: fileid3) -> Result<nfspath3, nfsstat3> {
        let target = self.fs.readlink(id).await.map_err(nfs_status)?;
        Ok(nfsstring(target.as_os_str().as_bytes().to_vec()))
    }
}

/// A running loopback NFS server for one mount.
pub struct NfsServer {
    pub port: u16,
    task: tokio::task::JoinHandle<()>,
}

impl NfsServer {
    /// Serve `fs` on `127.0.0.1` at an ephemeral port.
    pub async fn start(fs: Arc<RemoteFs>) -> anyhow::Result<NfsServer> {
        let listener = NFSTcpListener::bind("127.0.0.1:0", NfsAdapter::new(fs)).await?;
        let port = listener.get_listen_port();
        let task = tokio::spawn(async move {
            if let Err(e) = listener.handle_forever().await {
                tracing::warn!("project mount NFS server stopped: {e}");
            }
        });
        Ok(NfsServer { port, task })
    }

    /// Stop accepting and drop the server. Call after the kernel unmounted (a still-mounted
    /// client would otherwise see its server vanish; a soft mount turns that into EIO).
    pub fn stop(self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_round_trip_at_millisecond_precision() {
        let t = nfstime(1_700_000_123_456);
        assert_eq!(t.seconds, 1_700_000_123);
        assert_eq!(t.nseconds, 456_000_000);
        assert_eq!(ms_of(t), 1_700_000_123_456);
    }

    #[test]
    fn sattr_maps_to_node_setattr() {
        let mut s = sattr3::default();
        assert_eq!(set_of(&s), SetAttr::default());
        s.mode = set_mode3::mode(0o100644);
        s.size = set_size3::size(0);
        s.mtime = set_mtime::SET_TO_SERVER_TIME;
        s.atime = set_atime::SET_TO_CLIENT_TIME(nfstime3 { seconds: 2, nseconds: 5_000_000 });
        let set = set_of(&s);
        assert_eq!(set.perm, Some(0o644));
        assert_eq!(set.size, Some(0));
        assert_eq!(set.mtime, Some(SetTime::Now));
        assert_eq!(set.atime, Some(SetTime::UnixMs(2_005)));
    }

    #[test]
    fn outages_are_io_errors_never_jukebox() {
        assert!(matches!(nfs_status(FsErr::TimedOut), nfsstat3::NFS3ERR_IO));
        assert!(matches!(nfs_status(FsErr::Unreachable("x".into())), nfsstat3::NFS3ERR_IO));
        assert!(matches!(nfs_status(FsErr::NotEmpty), nfsstat3::NFS3ERR_NOTEMPTY));
        assert!(matches!(nfs_status(FsErr::Stale), nfsstat3::NFS3ERR_STALE));
    }

    #[tokio::test]
    async fn readdir_pages_by_fileid() {
        use super::super::remote_fs::tests::FakeNode;
        use super::super::remote_fs::CacheConfig;
        let node = FakeNode::new("/r");
        for n in ["a", "b", "c"] {
            node.put_file(&format!("/r/{n}"), b"x");
        }
        let fs = Arc::new(RemoteFs::new(node, "/r".into(), CacheConfig::default()));
        let nfs = NfsAdapter::new(fs);
        let p1 = nfs.readdir(ROOT_ID, 0, 2).await.unwrap();
        assert_eq!(p1.entries.len(), 2);
        assert!(!p1.end);
        let last = p1.entries.last().unwrap().fileid;
        let p2 = nfs.readdir(ROOT_ID, last, 2).await.unwrap();
        assert_eq!(p2.entries.len(), 1);
        assert_eq!(p2.entries[0].name.0, b"c");
        assert!(p2.end);
        assert!(matches!(nfs.readdir(ROOT_ID, 9999, 2).await, Err(nfsstat3::NFS3ERR_BAD_COOKIE)));
        // Attributes are owned by this user.
        assert_eq!(p1.entries[0].attr.uid, unsafe { libc::getuid() });
    }
}
