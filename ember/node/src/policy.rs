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
    /// absolute path (a relative entry is ignored), with its nearest existing ancestor
    /// canonicalised and the missing tail appended, so it is matched against canonical candidates
    /// and against not-yet-existing write targets. Matching is by path, not by inode: a bind mount or hard link is not detected.
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
        let denied = denied
            .into_iter()
            .filter(|d| d.is_absolute())
            .map(|d| canonical_or_lexical(&d))
            .collect();
        Ok(Self { roots: out, denied })
    }

    /// Whether `canonical` equals or is inside a denied entry (component-wise).
    pub fn is_denied(&self, canonical: &Path) -> bool {
        self.denied.iter().any(|d| canonical.starts_with(d))
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
}
