//! The runtime dir (SPEC FR-A11, #784 answer 4): state that agents and editor servers must use,
//! kept out of the data dir so the agent sandbox can deny the whole data dir.
//!
//! It is the data dir's sibling `<parent>/<name>-runtime` (`~/.ember` gives `~/.ember-runtime`,
//! `EMBER_DATA_DIR=/srv/e` gives `/srv/e-runtime`). It holds:
//! - `agy/`: Antigravity session roots and the conversation index (agy itself reads them);
//! - `ose-data/`: the OSE data root, unless `EMBER_OSE_DATA_DIR` names another one.
//!
//! [`migrate`] runs once at server start: each of [`MOVED`] still in the data dir is renamed into
//! the runtime dir. It is idempotent: an entry already moved is skipped, and when both places hold
//! one the data dir copy is left alone with a warning (nothing is merged or deleted).

use std::path::{Path, PathBuf};

/// The entries moved from the data dir to the runtime dir.
pub const MOVED: &[&str] = &["agy", "ose-data"];

/// The runtime dir for `data_dir`: its sibling `<name>-runtime`.
pub fn runtime_dir(data_dir: &Path) -> PathBuf {
    let name = data_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "ember".into());
    let parent = data_dir.parent().map(Path::to_path_buf).unwrap_or_default();
    parent.join(format!("{name}-runtime"))
}

/// What [`migrate`] did with one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Moved {
    /// Renamed from the data dir.
    Moved(String),
    /// Present in both places; the data dir copy was left.
    Conflict(String),
}

/// Make `dir` (and parents) with mode 0700 when it does not exist.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Move each of [`MOVED`] from `data_dir` to `runtime`. A rename that fails (another file system)
/// is an error for that entry, logged by the caller; the others still run.
pub fn migrate(data_dir: &Path, runtime: &Path) -> Vec<Result<Moved, String>> {
    let mut out = Vec::new();
    for name in MOVED {
        let from = data_dir.join(name);
        if std::fs::symlink_metadata(&from).is_err() {
            continue;
        }
        let to = runtime.join(name);
        if std::fs::symlink_metadata(&to).is_ok() {
            out.push(Ok(Moved::Conflict(name.to_string())));
            continue;
        }
        let r = private_dir(runtime)
            .and_then(|_| std::fs::rename(&from, &to))
            .map(|_| Moved::Moved(name.to_string()))
            .map_err(|e| format!("moving {} to {}: {e}", from.display(), to.display()));
        out.push(r);
    }
    out
}

/// [`migrate`] with log lines, as the server runs it at start.
pub fn migrate_logged(data_dir: &Path, runtime: &Path) {
    for r in migrate(data_dir, runtime) {
        match r {
            Ok(Moved::Moved(n)) => tracing::info!(entry = %n, to = %runtime.display(), "moved out of the data dir (FR-A11)"),
            Ok(Moved::Conflict(n)) => tracing::warn!(
                entry = %n,
                data_dir = %data_dir.display(),
                runtime = %runtime.display(),
                "present in both the data dir and the runtime dir; the runtime dir copy is used, the old one is left"
            ),
            Err(e) => tracing::warn!("runtime dir migration: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_a11_runtime_dir_is_the_data_dir_sibling() {
        assert_eq!(runtime_dir(Path::new("/h/.ember")), PathBuf::from("/h/.ember-runtime"));
        assert_eq!(runtime_dir(Path::new("/srv/e")), PathBuf::from("/srv/e-runtime"));
    }

    #[test]
    fn fr_a11_migration_moves_once_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join(".ember");
        let rt = runtime_dir(&data);
        std::fs::create_dir_all(data.join("agy/sessions/s1")).unwrap();
        std::fs::write(data.join("agy/sessions/s1/settings.json"), "{}").unwrap();
        std::fs::create_dir_all(data.join("ose-data/local")).unwrap();
        std::fs::write(data.join("ember.db"), "db").unwrap();

        let r = migrate(&data, &rt);
        assert_eq!(r, vec![Ok(Moved::Moved("agy".into())), Ok(Moved::Moved("ose-data".into()))]);
        assert!(rt.join("agy/sessions/s1/settings.json").is_file());
        assert!(rt.join("ose-data/local").is_dir());
        assert!(!data.join("agy").exists() && !data.join("ose-data").exists());
        assert!(data.join("ember.db").is_file(), "only the listed entries move");

        // Second start: nothing to do.
        assert!(migrate(&data, &rt).is_empty());

        // An old copy reappears beside the moved one: left alone, reported.
        std::fs::create_dir_all(data.join("agy")).unwrap();
        assert_eq!(migrate(&data, &rt), vec![Ok(Moved::Conflict("agy".into()))]);
        assert!(data.join("agy").is_dir() && rt.join("agy/sessions/s1").is_dir());
    }

    #[test]
    fn fr_a11_migration_with_nothing_to_move_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("d");
        std::fs::create_dir_all(&data).unwrap();
        assert!(migrate(&data, &runtime_dir(&data)).is_empty());
        assert!(!runtime_dir(&data).exists());
    }
}
