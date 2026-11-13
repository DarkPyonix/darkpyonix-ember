//! The received-files folder check shared by the server and the node (SPEC `FR-F12`, `FR-F13`).

use std::path::{Path, PathBuf};

/// Why the data folder check refused a folder (the server prefixes it with `invalid received_dir:`).
pub const REFUSED_DATA_FOLDER: &str =
    "the data folder, anything inside it except received/, and its parents are refused";

/// Checks a requested received-files folder and returns its canonical path.
///
/// The folder must be absolute, must not be a symlink, must be a directory (created when
/// missing), and its canonical path must not be `data_dir`, inside it (except
/// `<data_dir>/received`), or a parent of it. The checks run on the canonical path so `..` and
/// symlinked parents cannot reach a refused place. `data_dir` is expected canonical. The error is
/// the reason, in English.
pub fn check(data_dir: &Path, dir: &Path) -> Result<PathBuf, String> {
    if !dir.is_absolute() {
        return Err("must be an absolute path".into());
    }
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_symlink() => return Err("must not be a symlink".into()),
        Ok(m) if !m.is_dir() => return Err("not a directory".into()),
        Ok(_) => {}
        Err(_) => {
            // Check the would-be location before creating anything.
            let mut probe = dir.to_path_buf();
            let mut tail = Vec::new();
            while !probe.exists() {
                match (probe.file_name().map(|n| n.to_os_string()), probe.parent()) {
                    (Some(n), Some(p)) => {
                        tail.push(n);
                        probe = p.to_path_buf();
                    }
                    _ => return Err("cannot be created".into()),
                }
            }
            let mut planned = std::fs::canonicalize(&probe).map_err(|e| e.to_string())?;
            for n in tail.into_iter().rev() {
                planned.push(n);
            }
            check_canonical(data_dir, &planned)?;
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
    }
    let canonical = std::fs::canonicalize(dir).map_err(|e| e.to_string())?;
    check_canonical(data_dir, &canonical)?;
    Ok(canonical)
}

fn check_canonical(data_dir: &Path, canonical: &Path) -> Result<(), String> {
    if canonical == data_dir.join("received") {
        return Ok(());
    }
    if canonical.starts_with(data_dir) || data_dir.starts_with(canonical) {
        return Err(REFUSED_DATA_FOLDER.into());
    }
    Ok(())
}
