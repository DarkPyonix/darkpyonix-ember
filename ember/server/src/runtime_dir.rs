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

use std::path::Path;

/// The entries moved from the data dir to the runtime dir.
pub const MOVED: &[&str] = &["agy", "ose-data"];

/// The runtime dir for `data_dir`: its sibling `<name>-runtime`. The rule lives beside the OSE
/// manager, which `ember-node` runs too (#896).
pub use ember_node::ose::runtime_dir;

/// The server's data folder over the variables `var` returns (an empty value counts as unset):
/// `EMBER_DATA_DIR`, else `<home>/.ember` where `<home>` is `HOME`, or `USERPROFILE` when `HOME`
/// is unset or empty (a Windows program started from the Start menu has no `HOME`). It is the
/// folder the app looks in for `server-token` and the owner door (FR-S11, FR-S12, #1042). With
/// neither set it is the relative `.ember`, as before.
pub fn data_dir_from_vars(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> std::path::PathBuf {
    let path = |name: &str| var(name).filter(|v| !v.is_empty()).map(std::path::PathBuf::from);
    path("EMBER_DATA_DIR")
        .unwrap_or_else(|| path("HOME").or_else(|| path("USERPROFILE")).unwrap_or_default().join(".ember"))
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
    use std::path::PathBuf;

    #[test]
    fn fr_a11_runtime_dir_is_the_data_dir_sibling() {
        assert_eq!(runtime_dir(Path::new("/h/.ember")), PathBuf::from("/h/.ember-runtime"));
        assert_eq!(runtime_dir(Path::new("/srv/e")), PathBuf::from("/srv/e-runtime"));
    }

    #[test]
    fn fr_s12_data_dir_uses_userprofile_without_home() {
        use std::ffi::OsString;
        let vars = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| OsString::from(*v))
        };
        assert_eq!(data_dir_from_vars(vars(&[("EMBER_DATA_DIR", "/d"), ("HOME", "/h")])), PathBuf::from("/d"));
        assert_eq!(data_dir_from_vars(vars(&[("HOME", "/h"), ("USERPROFILE", "/u")])), PathBuf::from("/h/.ember"));
        assert_eq!(data_dir_from_vars(vars(&[("HOME", ""), ("USERPROFILE", "/u")])), PathBuf::from("/u/.ember"));
        assert_eq!(data_dir_from_vars(vars(&[("EMBER_DATA_DIR", ""), ("USERPROFILE", "/u")])), PathBuf::from("/u/.ember"));
        assert_eq!(data_dir_from_vars(vars(&[])), PathBuf::from(".ember"));
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
