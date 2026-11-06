//! FR-W4 (#521): the tokens dir and its parent computer dir are hardened, not just created.
//!
//! A fake `dpx-ose-server` (a shell script) records that it ran. It exits at once, so the start
//! itself returns an error ("exited during startup"); what these tests check is whether the start
//! got past the token directory check (the binary ran) or was refused before writing any token.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use ember_server::ose::OseManager;

struct Fx {
    dir: tempfile::TempDir,
    manager: OseManager,
    computer: PathBuf,
    tokens: PathBuf,
}

/// Write an executable script: flush to disk and close the handle before chmod and use, because a
/// script still open for writing is "text file busy" (ETXTBSY) when executed (#546).
fn write_exec(path: &Path, content: &str) {
    {
        use std::io::Write;
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.sync_all().unwrap();
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn set_mode(p: &Path, mode: u32) {
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

/// A manager over `<tmp>/ose-data` with `local/` and `local/tokens/` already existing at the given
/// modes.
fn fx(computer_mode: u32, tokens_mode: u32) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("fake-ose");
    write_exec(&bin, "#!/bin/sh\necho ran >> \"$(dirname \"$0\")/runs.txt\"\n");
    let root = dir.path().join("ose-data");
    let computer = root.join("local");
    let tokens = computer.join("tokens");
    std::fs::create_dir_all(&tokens).unwrap();
    set_mode(&root, 0o700);
    set_mode(&tokens, tokens_mode);
    set_mode(&computer, computer_mode);
    let manager = OseManager::with_data_root(Some(bin), root);
    Fx { dir, manager, computer, tokens }
}

fn start(f: &Fx) -> Result<(), String> {
    let folder = f.dir.path().join("project");
    std::fs::create_dir_all(&folder).unwrap();
    f.manager.connection(&folder).map(drop)
}

fn ran(f: &Fx) -> bool {
    f.dir.path().join("runs.txt").exists()
}

fn token_files(f: &Fx) -> Vec<PathBuf> {
    std::fs::read_dir(&f.tokens).unwrap().map(|e| e.unwrap().path()).collect()
}

#[test]
fn fr_w4_ose_token_dir_group_and_other_writable_tokens_dir_is_refused() {
    let f = fx(0o700, 0o777);
    let err = start(&f).unwrap_err();
    assert!(err.contains("is writable by others; fix the permissions"), "{err}");
    assert!(err.contains(f.tokens.to_str().unwrap()), "{err}");
    assert!(!ran(&f), "the binary must not run");
    assert!(token_files(&f).is_empty(), "no token file may be written");
    assert_eq!(mode(&f.tokens), 0o777, "a refused dir is left as it was");
}

#[test]
fn fr_w4_ose_token_dir_group_writable_only_is_refused_too() {
    let f = fx(0o700, 0o770);
    assert!(start(&f).unwrap_err().contains("writable by others"));
    let f = fx(0o700, 0o702);
    assert!(start(&f).unwrap_err().contains("writable by others"));
    assert!(token_files(&f).is_empty());
}

#[test]
fn fr_w4_ose_token_dir_readable_tokens_dir_is_tightened_to_0700() {
    let f = fx(0o700, 0o755);
    let r = start(&f);
    assert!(ran(&f), "the start went past the token dir check: {r:?}");
    assert_eq!(mode(&f.tokens), 0o700);
}

#[test]
fn fr_w4_ose_token_dir_private_tokens_dir_works() {
    let f = fx(0o700, 0o700);
    let r = start(&f);
    assert!(ran(&f), "{r:?}");
    assert_eq!(mode(&f.tokens), 0o700);
    assert_eq!(mode(&f.computer), 0o700);
}

#[test]
fn fr_w4_ose_token_dir_writable_computer_dir_is_refused() {
    let f = fx(0o775, 0o700);
    let err = start(&f).unwrap_err();
    assert!(err.contains("is writable by others; fix the permissions"), "{err}");
    assert!(err.contains(f.computer.to_str().unwrap()), "{err}");
    assert!(!ran(&f));
    assert!(token_files(&f).is_empty());
    assert_eq!(mode(&f.computer), 0o775);
}

#[test]
fn fr_w4_ose_token_dir_readable_computer_dir_is_tightened() {
    let f = fx(0o755, 0o700);
    let r = start(&f);
    assert!(ran(&f), "{r:?}");
    assert_eq!(mode(&f.computer), 0o700);
}

#[test]
fn fr_w4_ose_token_dir_symlinked_tokens_dir_is_refused() {
    let f = fx(0o700, 0o700);
    let real = f.dir.path().join("elsewhere");
    std::fs::create_dir(&real).unwrap();
    set_mode(&real, 0o700);
    std::fs::remove_dir(&f.tokens).unwrap();
    std::os::unix::fs::symlink(&real, &f.tokens).unwrap();
    let err = start(&f).unwrap_err();
    assert!(err.contains("symlink"), "{err}");
    assert!(!ran(&f));
    assert!(std::fs::read_dir(&real).unwrap().next().is_none());
}
