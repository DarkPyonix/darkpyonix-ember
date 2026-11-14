//! FR-X1 (#468): the file API opens first and checks the opened handle, so a symlink swapped into
//! the path between the policy check and the open cannot make a read, a hash or a write land
//! outside the roots. Each test races a swapper thread against many operations.
//!
//! #456 adds the namespace operations (mkdir, remove, rename, symlink, stat, lstat, readlink,
//! list, glob): they open the parent directory, check the handle and act relative to it, so the
//! same parent swap cannot create, remove, move or reveal anything outside the root.
//! Written test first; not compiled by the author (sub-agents do not build).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ember_node::fs;
use ember_node::policy::PathPolicy;
use ember_node::proto::*;

const ROUNDS: usize = 3000;

struct Race {
    root: PathBuf,
    outside: PathBuf,
    stop: Arc<AtomicBool>,
    swapper: Option<std::thread::JoinHandle<()>>,
    _dir: tempfile::TempDir,
}

impl Drop for Race {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.swapper.take() {
            let _ = h.join();
        }
    }
}

/// `root/d` flips between a real directory (holding `f` = "inside") and a symlink to `outside`
/// (holding `f` = "SECRET"), as fast as the swapper can rename.
fn race() -> Race {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = base.join("root");
    let outside = base.join("outside");
    std::fs::create_dir_all(root.join("real")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(root.join("real/f"), "inside").unwrap();
    std::fs::write(outside.join("f"), "SECRET").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("evil")).unwrap();
    std::fs::rename(root.join("real"), root.join("d")).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let (r, s) = (root.clone(), stop.clone());
    let swapper = std::thread::spawn(move || {
        let (d, real, evil) = (r.join("d"), r.join("real"), r.join("evil"));
        while !s.load(Ordering::SeqCst) {
            // d is the real directory: park it, put the symlink in its place, then swap back.
            let _ = std::fs::rename(&d, &real);
            let _ = std::fs::rename(&evil, &d);
            std::thread::yield_now();
            let _ = std::fs::rename(&d, &evil);
            let _ = std::fs::rename(&real, &d);
            std::thread::yield_now();
        }
    });
    Race { root, outside, stop, swapper: Some(swapper), _dir: dir }
}

fn policy(root: &Path) -> PathPolicy {
    PathPolicy::new([root.to_path_buf()]).unwrap()
}

#[test]
fn fr_x1_read_never_returns_a_file_outside_the_root_while_a_symlink_is_swapped() {
    let r = race();
    let p = policy(&r.root);
    let mut ok = 0;
    for _ in 0..ROUNDS {
        let req = ReadRequest { path: r.root.join("d/f"), offset: 0, len: None, hash: true };
        if let Ok(resp) = fs::read(&p, &req) {
            assert_eq!(resp.data, b"inside", "a read escaped the root");
            assert_eq!(resp.sha256.as_deref(), Some(fs::sha256_bytes(b"inside").as_str()), "the hash escaped the root");
            assert!(resp.path.starts_with(&r.root), "{:?}", resp.path);
            ok += 1;
        }
    }
    // Some reads must succeed, or the test proves nothing about the swap window.
    assert!(ok > 0, "no read succeeded");
}

#[test]
fn fr_x1_grep_never_reads_a_file_outside_the_root_while_a_symlink_is_swapped() {
    let r = race();
    let p = policy(&r.root);
    for _ in 0..ROUNDS / 10 {
        let req = GrepRequest {
            root: r.root.join("d/f"),
            pattern: "SECRET".into(),
            glob: None,
            case_insensitive: false,
            respect_ignore: false,
            limit: None,
        };
        if let Ok(resp) = fs::grep(&p, &req) {
            assert!(resp.matches.is_empty(), "grep read outside the root: {:?}", resp.matches);
        }
    }
}

#[test]
fn fr_x1_write_never_lands_outside_the_root_while_a_symlink_is_swapped() {
    let r = race();
    let p = policy(&r.root);
    for i in 0..ROUNDS / 3 {
        let req = WriteRequest {
            path: r.root.join(format!("d/new-{i}")),
            data: b"planted".to_vec(),
            expect: None,
            create_parents: false,
        };
        if let Ok(resp) = fs::write(&p, &req) {
            assert!(resp.path.starts_with(&r.root), "{:?}", resp.path);
        }
    }
    r.stop.store(true, Ordering::SeqCst);
    let escaped: Vec<_> = std::fs::read_dir(&r.outside)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .filter(|n| n != "f")
        .collect();
    assert!(escaped.is_empty(), "files written outside the root: {escaped:?}");
}

#[test]
fn fr_x1_a_final_symlink_swapped_in_is_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(base.join("secret"), "SECRET").unwrap();
    std::fs::write(root.join("f"), "inside").unwrap();
    let p = policy(&root);
    let stop = Arc::new(AtomicBool::new(false));
    let (r2, s) = (root.clone(), stop.clone());
    let secret = base.join("secret");
    let swapper = std::thread::spawn(move || {
        let (f, keep, link) = (r2.join("f"), r2.join("keep"), r2.join("link"));
        let _ = std::os::unix::fs::symlink(&secret, &link);
        while !s.load(Ordering::SeqCst) {
            let _ = std::fs::rename(&f, &keep);
            let _ = std::fs::rename(&link, &f);
            std::thread::yield_now();
            let _ = std::fs::rename(&f, &link);
            let _ = std::fs::rename(&keep, &f);
        }
    });
    for _ in 0..ROUNDS {
        let req = ReadRequest { path: root.join("f"), offset: 0, len: None, hash: true };
        if let Ok(resp) = fs::read(&p, &req) {
            assert_eq!(resp.data, b"inside");
        }
        let pw = PwriteRequest { path: root.join("f"), offset: 0, data: b"inside".to_vec() };
        let _ = fs::pwrite(&p, &pw);
    }
    stop.store(true, Ordering::SeqCst);
    swapper.join().unwrap();
    assert_eq!(std::fs::read(base.join("secret")).unwrap(), b"SECRET", "pwrite followed a swapped symlink");
}

/// The names in `outside` now (after the swapper stopped).
fn outside_names(r: &Race) -> Vec<String> {
    r.stop.store(true, Ordering::SeqCst);
    let mut v: Vec<String> = std::fs::read_dir(&r.outside)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn fr_x1_mkdir_never_creates_outside_the_root_while_a_parent_is_swapped() {
    let r = race();
    let p = policy(&r.root);
    for i in 0..ROUNDS / 3 {
        let one = MkdirRequest { path: r.root.join(format!("d/one-{i}")), parents: false, mode: None };
        if let Ok(s) = fs::mkdir(&p, &one) {
            assert!(s.path.starts_with(&r.root), "{:?}", s.path);
        }
        let deep = MkdirRequest { path: r.root.join(format!("d/deep-{i}/x/y")), parents: true, mode: None };
        if let Ok(s) = fs::mkdir(&p, &deep) {
            assert!(s.path.starts_with(&r.root), "{:?}", s.path);
        }
    }
    assert_eq!(outside_names(&r), vec!["f".to_string()], "mkdir created a directory outside the root");
}

#[test]
fn fr_x1_symlink_is_never_planted_outside_the_root_while_a_parent_is_swapped() {
    let r = race();
    let p = policy(&r.root);
    for i in 0..ROUNDS / 3 {
        let req = SymlinkRequest { path: r.root.join(format!("d/l-{i}")), target: "f".into() };
        if let Ok(s) = fs::symlink(&p, &req) {
            assert!(s.path.starts_with(&r.root), "{:?}", s.path);
        }
    }
    assert_eq!(outside_names(&r), vec!["f".to_string()], "symlink planted a link outside the root");
}

#[test]
fn fr_x1_symlink_target_outside_the_roots_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = base.join("root");
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::write(base.join("secret"), "SECRET").unwrap();
    let p = policy(&root);
    for target in [base.join("secret"), PathBuf::from("../../secret"), PathBuf::from("/etc/hosts")] {
        let req = SymlinkRequest { path: root.join("a/l"), target: target.clone() };
        let e = fs::symlink(&p, &req).unwrap_err();
        assert_eq!(e.errno(), Some("EACCES"), "{target:?}: {e}");
        assert!(std::fs::symlink_metadata(root.join("a/l")).is_err(), "{target:?}: a link was made");
    }
    // Inside: relative, relative through `..`, absolute, not existing yet.
    for (i, target) in [PathBuf::from("f"), PathBuf::from("../a/g"), root.join("a/h")].into_iter().enumerate() {
        let req = SymlinkRequest { path: root.join(format!("a/ok-{i}")), target };
        assert_eq!(fs::symlink(&p, &req).unwrap().kind, FileKind::Symlink);
    }
}

#[test]
fn fr_x1_remove_never_deletes_outside_the_root_while_a_parent_is_swapped() {
    let r = race();
    let n = ROUNDS / 3;
    for i in 0..n {
        std::fs::write(r.outside.join(format!("v-{i}")), "victim").unwrap();
        std::fs::create_dir_all(r.outside.join(format!("t-{i}/sub"))).unwrap();
        std::fs::write(r.outside.join(format!("t-{i}/sub/x")), "victim").unwrap();
    }
    let p = policy(&r.root);
    for i in 0..n {
        let _ = fs::remove(&p, &RemoveRequest { path: r.root.join(format!("d/v-{i}")), recursive: false });
        let _ = fs::remove(&p, &RemoveRequest { path: r.root.join(format!("d/t-{i}")), recursive: true });
    }
    r.stop.store(true, Ordering::SeqCst);
    for i in 0..n {
        assert!(r.outside.join(format!("v-{i}")).exists(), "remove deleted outside/v-{i}");
        assert!(r.outside.join(format!("t-{i}/sub/x")).exists(), "recursive remove deleted inside outside/t-{i}");
    }
    assert!(r.outside.join("f").exists());
}

#[test]
fn fr_x1_rename_never_moves_into_or_out_of_the_outside_while_a_parent_is_swapped() {
    let r = race();
    let n = ROUNDS / 3;
    for i in 0..n {
        std::fs::write(r.outside.join(format!("steal-{i}")), "SECRET").unwrap();
        std::fs::write(r.root.join(format!("src-{i}")), "inside").unwrap();
    }
    let p = policy(&r.root);
    for i in 0..n {
        // Out of the swapped parent (would steal an outside file into the root).
        let _ = fs::rename(
            &p,
            &RenameRequest { from: r.root.join(format!("d/steal-{i}")), to: r.root.join(format!("got-{i}")), overwrite: true },
        );
        // Into the swapped parent (would move an inside file outside).
        let _ = fs::rename(
            &p,
            &RenameRequest { from: r.root.join(format!("src-{i}")), to: r.root.join(format!("d/dst-{i}")), overwrite: true },
        );
    }
    let names = outside_names(&r);
    for i in 0..n {
        assert!(!r.root.join(format!("got-{i}")).exists(), "rename moved outside/steal-{i} into the root");
        assert!(names.contains(&format!("steal-{i}")), "outside/steal-{i} was moved");
    }
    assert!(!names.iter().any(|n| n.starts_with("dst-")), "rename moved files outside: {names:?}");
}

#[test]
fn fr_x1_stat_lstat_readlink_list_glob_never_reveal_outside_while_a_parent_is_swapped() {
    let r = race();
    std::fs::write(r.outside.join("only-outside"), "SECRET").unwrap();
    std::os::unix::fs::symlink("/the/outside/link/target", r.outside.join("lnk")).unwrap();
    let outside_ino = {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(r.outside.join("f")).unwrap().ino()
    };
    let p = policy(&r.root);
    let mut listed = 0;
    for _ in 0..ROUNDS / 3 {
        assert!(fs::stat(&p, &r.root.join("d/only-outside")).is_err(), "stat reached outside");
        assert!(fs::lstat(&p, &r.root.join("d/only-outside")).is_err(), "lstat reached outside");
        assert!(fs::readlink(&p, &r.root.join("d/lnk")).is_err(), "readlink reached outside");
        if let Ok(s) = fs::stat(&p, &r.root.join("d/f")) {
            assert_ne!(s.ino, Some(outside_ino), "stat answered for the outside file");
        }
        if let Ok(s) = fs::lstat(&p, &r.root.join("d/f")) {
            assert_ne!(s.ino, Some(outside_ino), "lstat answered for the outside file");
        }
        if let Ok(l) = fs::list(&p, &r.root.join("d")) {
            assert!(
                !l.entries.iter().any(|e| e.name == "only-outside" || e.name == "lnk"),
                "list showed outside entries: {:?}",
                l.entries.iter().map(|e| &e.name).collect::<Vec<_>>()
            );
            listed += 1;
        }
        let g = GlobRequest { root: r.root.clone(), pattern: "**/only-outside".into(), respect_ignore: false, limit: None };
        if let Ok(g) = fs::glob(&p, &g) {
            assert!(g.paths.is_empty(), "glob found outside paths: {:?}", g.paths);
        }
    }
    assert!(listed > 0, "no list succeeded");
}
