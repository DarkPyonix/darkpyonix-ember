//! Path policy: every path an operation touches must resolve inside an allowed root.
//!
//! Paths must be absolute. They are canonicalised (symlinks and `..` resolved) before the check,
//! so a symlink inside a root that points outside is rejected. For a path that does not exist yet
//! (a write target), the nearest existing ancestor is canonicalised and the remaining components
//! are appended; those may not contain `..`.
//!
//! Roots can carry **denied** paths (see [`PathPolicy::with_denied`]): a canonical path that equals
//! or is inside a denied entry is refused with the same error as a path outside the roots, so the
//! refusal does not reveal that the path exists or why. This narrows the file API and command
//! working directories only (for example it keeps the local node's `/fs` away from the server's
//! own data folder).
//!
//! This confines the file API and command working directories. It is **not** a sandbox for the
//! commands themselves: a command can touch anything its user can.
//!
//! **Check, then open (#468).** Resolving a path and opening it are two steps, so a symlink swapped
//! into the path between them could make the open land outside the roots. The file operations
//! therefore open first and check the opened handle with [`PathPolicy::check_opened`]: the path is
//! resolved again after the open and, on Unix, the opened file's device and inode must equal those
//! of the resolved path. A handle that does not match is refused like a path outside the roots.
//! The namespace operations (mkdir, remove, rename, symlink, stat, lstat, readlink, list) check
//! a directory handle the same way and act relative to it (#456); see `crate::fs`.

use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("path must be absolute: {0}")]
    NotAbsolute(PathBuf),
    #[error("path is outside the allowed roots: {0}")]
    Outside(PathBuf),
    #[error("{0}: {1}")]
    Io(PathBuf, io::Error),
}

#[derive(Debug, Clone)]
pub struct PathPolicy {
    roots: Vec<PathBuf>,
    /// Canonical when the entry existed at construction, else its nearest existing ancestor made
    /// canonical plus the missing tail.
    denied: Vec<PathBuf>,
    /// The denied entries as configured (absolute), resolved again at every check (#534).
    denied_given: Vec<PathBuf>,
}

impl PathPolicy {
    /// Canonicalises each root; a root that does not exist is an error.
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> anyhow::Result<Self> {
        Self::with_denied(roots, [])
    }

    /// Like [`PathPolicy::new`] plus denied paths inside (or anywhere relative to) the roots.
    ///
    /// A path is refused when its canonical form equals a denied entry or is inside it
    /// (component-wise: `/h/.ember-other` is not inside `/h/.ember`). A denied entry that exists
    /// is canonicalised at construction, so a symlink into it, `..` into it and the entry reached
    /// through another name are all refused. A denied entry that does not exist yet is kept as its
    /// nearest existing ancestor canonicalised with the missing tail appended, so it is matched
    /// against canonical candidates and against not-yet-existing write targets.
    ///
    /// Every entry is also resolved again at each check (#534): an entry created after
    /// construction, or a symlink planted at its place later, is matched by its canonical form
    /// from then on. A case variant (`.Ember` for `.ember`) on a case-insensitive volume is caught
    /// only once the entry exists, because canonicalising an existing path returns its stored
    /// name; a missing entry is matched by its bytes.
    ///
    /// A relative entry is an error (#534): it would be resolved against the working directory
    /// of whoever reads it, so dropping it would fail open and keeping it would be ambiguous.
    /// Matching is by path, not by inode: a bind mount or hard link is not detected.
    pub fn with_denied(
        roots: impl IntoIterator<Item = PathBuf>,
        denied: impl IntoIterator<Item = PathBuf>,
    ) -> anyhow::Result<Self> {
        let mut out = Vec::new();
        for r in roots {
            let c = std::fs::canonicalize(&r)
                .map_err(|e| anyhow::anyhow!("allowed root {}: {e}", r.display()))?;
            out.push(c);
        }
        anyhow::ensure!(!out.is_empty(), "at least one allowed root is required");
        let denied_given: Vec<PathBuf> = denied.into_iter().collect();
        if let Some(rel) = denied_given.iter().find(|d| !d.is_absolute()) {
            anyhow::bail!("denied path must be absolute: {}", rel.display());
        }
        let denied = denied_given.iter().map(|d| canonical_or_lexical(d)).collect();
        Ok(Self { roots: out, denied, denied_given })
    }

    /// Whether `canonical` equals or is inside a denied entry (component-wise), as resolved at
    /// construction or now.
    pub fn is_denied(&self, canonical: &Path) -> bool {
        self.denied.iter().any(|d| canonical.starts_with(d))
            || self.denied_given.iter().any(|d| canonical.starts_with(canonical_or_lexical(d)))
    }

    /// Check an already opened handle (#468). `path` is the canonical path the caller opened. It
    /// is resolved again and checked against the roots; on Unix the opened file must also be the
    /// file that path names now (same device and inode). Otherwise a symlink was swapped in
    /// between the check and the open, and the handle is refused like a path outside the roots.
    ///
    /// On Windows only the second resolution is done (the standard library has no stable file
    /// id), so a swap that is undone before that resolution is not detected there.
    pub fn check_opened(&self, file: &std::fs::File, path: &Path) -> Result<PathBuf, PolicyError> {
        let again = self.resolve_existing(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let opened = file.metadata().map_err(|e| PolicyError::Io(path.to_path_buf(), e))?;
            let named = std::fs::metadata(&again).map_err(|e| PolicyError::Io(path.to_path_buf(), e))?;
            if opened.dev() != named.dev() || opened.ino() != named.ino() {
                return Err(PolicyError::Outside(path.to_path_buf()));
            }
        }
        #[cfg(not(unix))]
        let _ = file;
        Ok(again)
    }

    pub fn denied(&self) -> &[PathBuf] {
        &self.denied
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    fn check(&self, canonical: PathBuf, original: &Path) -> Result<PathBuf, PolicyError> {
        if self.roots.iter().any(|r| canonical.starts_with(r)) && !self.is_denied(&canonical) {
            Ok(canonical)
        } else {
            Err(PolicyError::Outside(original.to_path_buf()))
        }
    }

    /// Resolve a path that must exist.
    pub fn resolve_existing(&self, path: &Path) -> Result<PathBuf, PolicyError> {
        if !path.is_absolute() {
            return Err(PolicyError::NotAbsolute(path.to_path_buf()));
        }
        let c = std::fs::canonicalize(path).map_err(|e| PolicyError::Io(path.to_path_buf(), e))?;
        // realpath(3) on macOS drops a trailing slash after a file; POSIX says ENOTDIR.
        if path.as_os_str().as_encoded_bytes().ends_with(b"/") && !c.is_dir() {
            let e = std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a directory");
            return Err(PolicyError::Io(path.to_path_buf(), e));
        }
        self.check(c, path)
    }

    /// Resolve a path that may not exist yet. Returns the canonical target path (if the final
    /// component is a symlink, the path it points to).
    pub fn resolve_for_write(&self, path: &Path) -> Result<PathBuf, PolicyError> {
        if !path.is_absolute() {
            return Err(PolicyError::NotAbsolute(path.to_path_buf()));
        }
        match std::fs::canonicalize(path) {
            Ok(c) => return self.check(c, path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(PolicyError::Io(path.to_path_buf(), e)),
        }
        // `..` through a missing directory cannot be resolved by the filesystem; refuse it.
        if path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(PolicyError::Outside(path.to_path_buf()));
        }
        // Walk up to the nearest existing ancestor.
        let mut tail = Vec::new();
        let mut cur = path;
        let base = loop {
            let parent = cur.parent().ok_or_else(|| PolicyError::Outside(path.to_path_buf()))?;
            match cur.file_name() {
                Some(n) => tail.push(n.to_os_string()),
                None => return Err(PolicyError::Outside(path.to_path_buf())),
            }
            match std::fs::canonicalize(parent) {
                Ok(c) => break c,
                Err(e) if e.kind() == io::ErrorKind::NotFound => cur = parent,
                Err(e) => return Err(PolicyError::Io(path.to_path_buf(), e)),
            }
        };
        let mut full = base;
        for n in tail.into_iter().rev() {
            full.push(n);
        }
        self.check(full, path)
    }

    /// Resolve a path **without following its final component**: the parent directory must
    /// exist and is canonicalised; the final name is appended as given. The entry itself may or
    /// may not exist. Used where the operation acts on a symbolic link itself (`lstat`,
    /// `readlink`, `remove`, `rename`, `symlink`), so a link inside a root that points outside
    /// is still visible and removable, but never followed.
    pub fn resolve_nofollow(&self, path: &Path) -> Result<PathBuf, PolicyError> {
        if !path.is_absolute() {
            return Err(PolicyError::NotAbsolute(path.to_path_buf()));
        }
        if path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(PolicyError::Outside(path.to_path_buf()));
        }
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            // `/` itself.
            return self.resolve_existing(path);
        };
        let base = std::fs::canonicalize(parent).map_err(|e| PolicyError::Io(path.to_path_buf(), e))?;
        self.check(base.join(name), path)
    }

    /// Check the target of a symbolic link about to be created in the directory `link_dir`
    /// (canonical) (#456). A relative target is read relative to `link_dir`, as the system will
    /// read it. The target may not exist yet: its longest existing prefix is canonicalised (so
    /// `..` and links in it are resolved as the system resolves them) and the missing rest may
    /// not contain `..`. The result must be inside the roots and outside every denied entry, so
    /// the file API cannot plant a link that points out of the roots. Every refusal is
    /// [`PolicyError::Outside`] naming the target. Returns the resolved target.
    ///
    /// This is a check at creation time: every later operation that follows the link resolves
    /// it again, so a target that moves out afterwards is still refused there.
    pub fn check_link_target(&self, link_dir: &Path, target: &Path) -> Result<PathBuf, PolicyError> {
        let outside = || PolicyError::Outside(target.to_path_buf());
        let full = if target.is_absolute() { target.to_path_buf() } else { link_dir.join(target) };
        let comps: Vec<Component> = full.components().collect();
        for i in (1..=comps.len()).rev() {
            let prefix: PathBuf = comps[..i].iter().collect();
            match std::fs::canonicalize(&prefix) {
                Ok(mut c) => {
                    let tail = &comps[i..];
                    if tail.iter().any(|t| matches!(t, Component::ParentDir)) {
                        return Err(outside());
                    }
                    for t in tail {
                        c.push(t);
                    }
                    return self.check(c, target).map_err(|_| outside());
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => return Err(outside()),
            }
        }
        Err(outside())
    }

    /// Whether `canonical` is one of the roots themselves (which must not be removed or moved).
    pub fn is_root(&self, canonical: &Path) -> bool {
        self.roots.iter().any(|r| r == canonical)
    }
}

/// `path` canonicalised; when it does not exist, its nearest existing ancestor canonicalised with
/// the missing tail appended unchanged.
fn canonical_or_lexical(path: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(path) {
        return c;
    }
    let mut tail = Vec::new();
    let mut cur = path;
    while let (Some(parent), Some(name)) = (cur.parent(), cur.file_name()) {
        tail.push(name.to_os_string());
        if let Ok(mut base) = std::fs::canonicalize(parent) {
            for n in tail.into_iter().rev() {
                base.push(n);
            }
            return base;
        }
        cur = parent;
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn rejects_dotdot_and_symlink_escape() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("root");
        let out = t.path().join("out");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("f"), b"x").unwrap();
        std::os::unix::fs::symlink(&out, root.join("link")).unwrap();
        let p = PathPolicy::new([root.clone()]).unwrap();

        assert!(p.resolve_existing(&root.join("../out/f")).is_err());
        assert!(p.resolve_existing(&root.join("link/f")).is_err());
        assert!(p.resolve_for_write(&root.join("link/new")).is_err());
        assert!(p.resolve_for_write(&root.join("a/../../out/new")).is_err());
        assert!(p.resolve_existing(Path::new("relative")).is_err());
        let ok = p.resolve_for_write(&root.join("a/b/new.txt")).unwrap();
        assert!(ok.ends_with("root/a/b/new.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn denied_paths_are_refused_like_outside_paths() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        let root = base.join("root");
        std::fs::create_dir_all(root.join(".ember/sub")).unwrap();
        std::fs::create_dir_all(root.join(".ember-other")).unwrap();
        std::fs::create_dir_all(root.join("work")).unwrap();
        std::os::unix::fs::symlink(root.join(".ember"), root.join("work/link")).unwrap();
        let p = PathPolicy::with_denied([root.clone()], [root.join(".ember"), root.join("missing-yet")]).unwrap();

        for bad in [
            root.join(".ember"),
            root.join(".ember/sub"),
            root.join(".ember/new"),
            root.join("work/link"),
            root.join("work/link/sub"),
            root.join("work/../.ember"),
        ] {
            assert!(matches!(p.resolve_for_write(&bad), Err(PolicyError::Outside(_))), "{bad:?}");
            assert!(matches!(p.resolve_nofollow(&bad), Err(PolicyError::Outside(_))) || bad.ends_with("link"), "{bad:?}");
        }
        assert!(matches!(p.resolve_existing(&root.join(".ember/sub")), Err(PolicyError::Outside(_))));
        assert!(matches!(p.resolve_existing(&root.join("work/link")), Err(PolicyError::Outside(_))));
        // A missing denied entry is matched lexically.
        assert!(matches!(p.resolve_for_write(&root.join("missing-yet/f")), Err(PolicyError::Outside(_))));
        // Siblings and prefix lookalikes are fine.
        assert!(p.resolve_existing(&root.join(".ember-other")).is_ok());
        assert!(p.resolve_existing(&root.join("work")).is_ok());
        assert!(p.resolve_for_write(&root.join(".ember-other/new")).is_ok());
        // No denied entries: unchanged.
        let q = PathPolicy::new([root.clone()]).unwrap();
        assert!(q.resolve_existing(&root.join(".ember/sub")).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn nofollow_keeps_the_final_link_but_checks_its_parent() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        let root = base.join("root");
        let out = base.join("out");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        std::os::unix::fs::symlink(&out, root.join("link")).unwrap();
        let p = PathPolicy::new([root.clone()]).unwrap();

        // The link itself is inside the root even though its target is not.
        assert_eq!(p.resolve_nofollow(&root.join("link")).unwrap(), root.join("link"));
        // A missing final component is fine; a missing parent is not.
        assert_eq!(p.resolve_nofollow(&root.join("new")).unwrap(), root.join("new"));
        assert!(matches!(p.resolve_nofollow(&root.join("a/new")), Err(PolicyError::Io(..))));
        // Going through the link as a directory follows it, so it is refused.
        assert!(matches!(p.resolve_nofollow(&root.join("link/x")), Err(PolicyError::Outside(_))));
        assert!(p.resolve_nofollow(&root.join("../out")).is_err());
        assert!(p.resolve_nofollow(Path::new("relative")).is_err());
        assert!(p.is_root(&root));
        assert!(!p.is_root(&root.join("link")));
    }

    #[test]
    fn fr_x1_relative_denied_entries_are_refused_not_dropped() {
        let t = tempfile::tempdir().unwrap();
        let e = PathPolicy::with_denied([t.path().to_path_buf()], [PathBuf::from(".ember")]).unwrap_err();
        assert!(e.to_string().contains("must be absolute"), "{e}");
    }

    #[cfg(unix)]
    #[test]
    fn fr_x1_denied_entries_are_resolved_again_at_check_time() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        let root = base.join("root");
        std::fs::create_dir_all(root.join("elsewhere/secret")).unwrap();
        // Missing at construction.
        let p = PathPolicy::with_denied([root.clone()], [root.join(".ember")]).unwrap();
        assert!(p.resolve_existing(&root.join("elsewhere/secret")).is_ok());
        // A symlink planted later at the denied place: its target is denied from then on.
        std::os::unix::fs::symlink(root.join("elsewhere"), root.join(".ember")).unwrap();
        assert!(matches!(p.resolve_existing(&root.join("elsewhere/secret")), Err(PolicyError::Outside(_))));
        assert!(matches!(p.resolve_existing(&root.join(".ember/secret")), Err(PolicyError::Outside(_))));
    }

    #[cfg(unix)]
    #[test]
    fn fr_x1_link_targets_must_resolve_inside_the_roots() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        let root = base.join("root");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join(".ember")).unwrap();
        std::fs::create_dir_all(base.join("out")).unwrap();
        std::os::unix::fs::symlink(base.join("out"), root.join("link-out")).unwrap();
        let p = PathPolicy::with_denied([root.clone()], [root.join(".ember")]).unwrap();
        let dir = root.join("a");
        // Inside: relative, absolute, missing yet, `..` that stays inside.
        assert!(p.check_link_target(&dir, Path::new("f.txt")).is_ok());
        assert!(p.check_link_target(&dir, &root.join("a/f.txt")).is_ok());
        assert!(p.check_link_target(&dir, Path::new("../a/new")).is_ok());
        // Outside: absolute, `..` out, through a link out, into a denied entry, `..` through a
        // missing directory.
        for bad in [
            base.join("out/secret"),
            PathBuf::from("../../out/secret"),
            PathBuf::from("../link-out/secret"),
            PathBuf::from("../.ember/server-token"),
            PathBuf::from("missing/../../../out"),
            PathBuf::from("/etc/hosts"),
        ] {
            assert!(matches!(p.check_link_target(&dir, &bad), Err(PolicyError::Outside(_))), "{bad:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn fr_x1_check_opened_refuses_a_handle_that_is_not_the_named_file() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a"), b"a").unwrap();
        std::fs::write(base.join("out"), b"o").unwrap();
        let p = PathPolicy::new([root.clone()]).unwrap();
        let inside = std::fs::File::open(root.join("a")).unwrap();
        assert_eq!(p.check_opened(&inside, &root.join("a")).unwrap(), root.join("a"));
        let outside = std::fs::File::open(base.join("out")).unwrap();
        assert!(matches!(p.check_opened(&outside, &root.join("a")), Err(PolicyError::Outside(_))));
    }
}
