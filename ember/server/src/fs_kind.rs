//! Whether a folder is on a local disk or on a network share, decided by the filesystem it is
//! mounted from, never by its path (SPEC FR-L31.24, #789).
//!
//! Code-OSS 1.139.1 guesses on macOS that every path below `/Volumes/` may be a network share
//! (`src/vs/platform/files/node/watcher/nodejs/nodejsWatcherLib.ts` L211) and refuses to watch it
//! with `fs.watch()`. `/Volumes/` is only where macOS mounts every disk that is not the boot disk,
//! so a local external APFS disk is caught by the guess. Ember asks the operating system instead:
//!
//! | OS | Source | Network when |
//! | -- | ------ | ------------ |
//! | macOS | `statfs(2)` `f_fstypename` | the name is in [`NETWORK_FS_NAMES`] |
//! | Linux | `statfs(2)` `f_type` | the magic is in [`NETWORK_FS_MAGICS`] |
//! | Windows | `GetDriveTypeW` on the volume root | the answer is `DRIVE_REMOTE` |
//!
//! Anything the operating system cannot answer is [`FsKind::Local`], because the usual case is a
//! local disk and native file events are the default.

use std::path::Path;

/// Where a folder's filesystem lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsKind {
    /// A disk of this machine: internal, or external (USB, Thunderbolt), whatever its mount point.
    Local,
    /// A filesystem served by another machine. Native file events do not report changes made on
    /// the other side.
    Network,
}

/// Filesystem type names of network filesystems, as `statfs` reports them in `f_fstypename`
/// (macOS) and as `/proc/mounts` names them (Linux).
pub const NETWORK_FS_NAMES: [&str; 12] =
    ["smbfs", "nfs", "nfs4", "afpfs", "webdav", "ftp", "cifs", "smb3", "ncpfs", "9p", "ceph", "afs"];

/// Linux `statfs` `f_type` magics of network filesystems (`linux/magic.h` and the filesystems'
/// own headers): NFS, SMB, CIFS, SMB2, NCP, Coda, AFS, 9p, Ceph.
pub const NETWORK_FS_MAGICS: [u32; 9] =
    [0x6969, 0x517B, 0xFF53_4D42, 0xFE53_4D42, 0x564C, 0x7375_7245, 0x5346_414F, 0x0102_1997, 0x00C3_6400];

/// Windows `GetDriveTypeW` answer for a network drive.
pub const DRIVE_REMOTE: u32 = 4;

/// Classify a filesystem type name (macOS `f_fstypename`). Case is ignored.
pub fn kind_of_fs_name(name: &str) -> FsKind {
    let name = name.trim().to_ascii_lowercase();
    if NETWORK_FS_NAMES.contains(&name.as_str()) { FsKind::Network } else { FsKind::Local }
}

/// Classify a Linux `statfs` `f_type` magic.
pub fn kind_of_fs_magic(magic: u32) -> FsKind {
    if NETWORK_FS_MAGICS.contains(&magic) { FsKind::Network } else { FsKind::Local }
}

/// Classify a Windows `GetDriveTypeW` answer.
pub fn kind_of_drive_type(drive_type: u32) -> FsKind {
    if drive_type == DRIVE_REMOTE { FsKind::Network } else { FsKind::Local }
}

/// The volume root `GetDriveTypeW` wants for a Windows path, with its trailing backslash:
/// `C:\` for a drive path and `\\server\share\` for a UNC path. The verbatim prefixes that
/// `std::fs::canonicalize` adds (`\\?\`, `\\?\UNC\`) are understood. `None` for anything else.
pub fn windows_volume_root(path: &str) -> Option<String> {
    let path = path.replace('/', "\\");
    let unc = if let Some(rest) = path.strip_prefix("\\\\?\\UNC\\") {
        Some(rest.to_string())
    } else if let Some(rest) = path.strip_prefix("\\\\?\\") {
        return drive_root(rest);
    } else {
        path.strip_prefix("\\\\").map(str::to_string)
    };
    match unc {
        Some(rest) => {
            let mut parts = rest.split('\\').filter(|p| !p.is_empty());
            let (server, share) = (parts.next()?, parts.next()?);
            Some(format!("\\\\{server}\\{share}\\"))
        }
        None => drive_root(&path),
    }
}

fn drive_root(path: &str) -> Option<String> {
    let mut chars = path.chars();
    let (letter, colon) = (chars.next()?, chars.next()?);
    (letter.is_ascii_alphabetic() && colon == ':').then(|| format!("{letter}:\\"))
}

/// The kind of the filesystem `path` is on. `path` must exist; an error or an operating system
/// without an answer gives [`FsKind::Local`].
pub fn fs_kind(path: &Path) -> FsKind {
    imp::fs_kind(path).unwrap_or(FsKind::Local)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn statfs(path: &Path) -> Option<libc::statfs> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: `c_path` is a NUL-terminated string and `st` is a writable `statfs` buffer; the
    // kernel fills it when the call returns 0.
    let rc = unsafe { libc::statfs(c_path.as_ptr(), st.as_mut_ptr()) };
    // SAFETY: `statfs` returned 0, so the buffer is initialised.
    (rc == 0).then(|| unsafe { st.assume_init() })
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    /// `f_fstypename` of the filesystem `path` is on, for example `apfs` or `smbfs`.
    pub fn fs_type_name(path: &Path) -> Option<String> {
        let st = statfs(path)?;
        let bytes: Vec<u8> = st.f_fstypename.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
        String::from_utf8(bytes).ok()
    }

    pub fn fs_kind(path: &Path) -> Option<FsKind> {
        fs_type_name(path).map(|name| kind_of_fs_name(&name))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;

    pub fn fs_kind(path: &Path) -> Option<FsKind> {
        // `f_type` is a signed word whose width depends on the target; the magics are 32-bit.
        #[allow(clippy::unnecessary_cast)]
        statfs(path).map(|st| kind_of_fs_magic(st.f_type as u32))
    }
}

#[cfg(windows)]
mod imp {
    use super::*;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetDriveTypeW(root_path_name: *const u16) -> u32;
    }

    pub fn fs_kind(path: &Path) -> Option<FsKind> {
        let root = windows_volume_root(&path.to_string_lossy())?;
        let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 string that lives across the call.
        Some(kind_of_drive_type(unsafe { GetDriveTypeW(wide.as_ptr()) }))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod imp {
    use super::*;

    pub fn fs_kind(_path: &Path) -> Option<FsKind> {
        None
    }
}

#[cfg(target_os = "macos")]
pub use imp::fs_type_name;

#[cfg(test)]
mod tests {
    use super::*;

    /// #789: the owner's projects are on `/Volumes/macMini`, a local external APFS disk.
    #[test]
    fn fr_l31_24_local_filesystems_are_local_whatever_the_mount_point() {
        for name in ["apfs", "hfs", "exfat", "msdos", "ntfs", "ext4", "btrfs", "xfs", "tmpfs", "APFS", ""] {
            assert_eq!(kind_of_fs_name(name), FsKind::Local, "{name}");
        }
    }

    #[test]
    fn fr_l31_24_network_filesystem_names_are_shares() {
        for name in ["smbfs", "nfs", "afpfs", "webdav", "cifs", "nfs4", "SMBFS"] {
            assert_eq!(kind_of_fs_name(name), FsKind::Network, "{name}");
        }
    }

    #[test]
    fn fr_l31_24_linux_magics_nfs_cifs_smb2_are_shares_and_ext4_is_local() {
        assert_eq!(kind_of_fs_magic(0x6969), FsKind::Network); // NFS_SUPER_MAGIC
        assert_eq!(kind_of_fs_magic(0xFF53_4D42), FsKind::Network); // CIFS_SUPER_MAGIC
        assert_eq!(kind_of_fs_magic(0xFE53_4D42), FsKind::Network); // SMB2_SUPER_MAGIC
        assert_eq!(kind_of_fs_magic(0x517B), FsKind::Network); // SMB_SUPER_MAGIC
        assert_eq!(kind_of_fs_magic(0xEF53), FsKind::Local); // EXT4_SUPER_MAGIC
        assert_eq!(kind_of_fs_magic(0x9123_683E), FsKind::Local); // BTRFS_SUPER_MAGIC
        assert_eq!(kind_of_fs_magic(0x0102_1994), FsKind::Local); // TMPFS_MAGIC
    }

    #[test]
    fn fr_l31_24_windows_only_drive_remote_is_a_share() {
        assert_eq!(kind_of_drive_type(DRIVE_REMOTE), FsKind::Network);
        // DRIVE_UNKNOWN, NO_ROOT_DIR, REMOVABLE, FIXED, CDROM, RAMDISK.
        for local in [0, 1, 2, 3, 5, 6] {
            assert_eq!(kind_of_drive_type(local), FsKind::Local, "{local}");
        }
    }

    #[test]
    fn fr_l31_24_windows_volume_root_of_drive_and_unc_paths() {
        assert_eq!(windows_volume_root(r"C:\Users\u\proj").as_deref(), Some(r"C:\"));
        assert_eq!(windows_volume_root(r"\\?\D:\proj").as_deref(), Some(r"D:\"));
        assert_eq!(windows_volume_root(r"\\nas\share\proj\a").as_deref(), Some(r"\\nas\share\"));
        assert_eq!(windows_volume_root(r"\\?\UNC\nas\share\proj").as_deref(), Some(r"\\nas\share\"));
        assert_eq!(windows_volume_root("e:/proj").as_deref(), Some(r"e:\"));
        assert_eq!(windows_volume_root(r"\\nas").as_deref(), None);
        assert_eq!(windows_volume_root("relative").as_deref(), None);
    }

    /// The real call on this machine: the temp dir is on a local disk on every CI runner.
    #[test]
    fn fr_l31_24_the_temp_dir_is_local() {
        assert_eq!(fs_kind(&std::env::temp_dir()), FsKind::Local);
    }

    #[test]
    fn fr_l31_24_a_missing_path_is_local() {
        assert_eq!(fs_kind(Path::new("/no/such/ember/path/789")), FsKind::Local);
    }

    /// On a Mac, a mounted volume below `/Volumes` that is not a network filesystem is local:
    /// the path prefix decides nothing. Skipped where no such volume is mounted.
    #[cfg(target_os = "macos")]
    #[test]
    fn fr_l31_24_a_local_disk_below_volumes_is_local() {
        let Ok(volumes) = std::fs::read_dir("/Volumes") else { return };
        for volume in volumes.flatten() {
            let path = volume.path();
            let Some(name) = fs_type_name(&path) else { continue };
            assert_eq!(fs_kind(&path), kind_of_fs_name(&name), "{}", path.display());
            if name == "apfs" || name == "hfs" || name == "exfat" {
                assert_eq!(fs_kind(&path), FsKind::Local, "{} ({name})", path.display());
            }
        }
    }
}
