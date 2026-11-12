//! FR-W4 (#521): the tokens dir and its parent computer dir are hardened, not just created.
//!
//! A fake `dpx-ose-server` (a shell script) records that it ran. It exits at once, so the start
//! itself returns an error ("exited during startup"); what these tests check is whether the start
//! got past the token directory check (the binary ran) or was refused before writing any token.
#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use ember_server::ose::OseManager;

struct Fx {
    dir: tempfile::TempDir,
    manager: OseManager,
    computer: PathBuf,
    tokens: PathBuf,
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
    common::write_exec_ready(&bin, "#!/bin/sh\necho ran >> \"$(dirname \"$0\")/runs.txt\"\n");
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

/// #521 item 2: a SIGKILL leaves `<hex>.token` behind; the startup sweep removes it and nothing
/// else (another file name, a symlink, a directory, an invalid computer dir).
#[test]
fn fr_w4_ose_token_dir_startup_sweep_removes_stale_token_files() {
    let f = fx(0o700, 0o700);
    let stale = f.tokens.join("0123456789abcdef.token");
    std::fs::write(&stale, "dead-token").unwrap();
    let other = f.tokens.join("notes.txt");
    std::fs::write(&other, "keep").unwrap();
    let outside = f.dir.path().join("outside.token");
    std::fs::write(&outside, "keep").unwrap();
    let link = f.tokens.join("fedcba9876543210.token");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let dir_named_token = f.tokens.join("aaaa.token");
    std::fs::create_dir(&dir_named_token).unwrap();
    let second = f.computer.parent().unwrap().join("node-2").join("tokens");
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(second.join("abcdef0123456789.token"), "dead").unwrap();

    assert_eq!(f.manager.sweep_stale_tokens(), 2);
    assert!(!stale.exists());
    assert!(!second.join("abcdef0123456789.token").exists());
    assert!(other.exists(), "a file that is not <hex>.token stays");
    assert!(std::fs::symlink_metadata(&link).is_ok(), "a symlink is not followed or removed");
    assert!(outside.exists(), "the symlink target is untouched");
    assert!(dir_named_token.is_dir());
    // A second sweep finds nothing; a manager without a root is a no-op.
    assert_eq!(f.manager.sweep_stale_tokens(), 0);
    let none = OseManager::with_data_root(None, f.dir.path().join("missing"));
    assert_eq!(none.sweep_stale_tokens(), 0);
}
