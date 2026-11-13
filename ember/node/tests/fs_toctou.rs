//! FR-X1 (#468): the file API opens first and checks the opened handle, so a symlink swapped into
//! the path between the policy check and the open cannot make a read, a hash or a write land
//! outside the roots. Each test races a swapper thread against many operations.
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
