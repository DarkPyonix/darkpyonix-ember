//! FR-L31.6a (#744): hot exit of the Ember 에디터 window. The backup format, where it lives, the
//! write after an edit, what is put back when the window opens, the conflict mark, removal on save,
//! revert and close, pruning by age and a corrupt file. The window itself is not driven here.

use std::path::{Path, PathBuf};

use ember_app::editor_backup::{
    backup_dir, backup_file_name, decode, encode, expired, load, prune_dir, restore_failed_notice, unreadable_notice, write_atomic,
    BackupEntry, BackupError, BackupFile, BackupKey, BaseMark, DocView, Flush, Loaded, Tracker, BACKUP_DEBOUNCE_MS, BACKUP_VERSION,
    DEFAULT_BACKUP_DAYS,
};
use ember_app::prefs::{self, Prefs};
use ember_app::ui::editor_states::Reopen;

const DAY_MS: u64 = 86_400_000;
const NOW: u64 = 1_800_000_000_000;

/// A folder under the repository's `.scratch/` (nothing a test makes goes outside the repository).
fn scratch() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch");
    std::fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}

fn key() -> BackupKey {
    BackupKey::new("mini", "ember", "/w")
}

fn entry(path: &str, text: &str, disk: &str) -> BackupEntry {
    BackupEntry { path: path.into(), text: text.into(), base: Some(BaseMark::of(disk)) }
}

fn file(entries: Vec<BackupEntry>, current: Option<&str>, written_at_ms: u64) -> BackupFile {
    BackupFile {
        version: BACKUP_VERSION,
        written_at_ms,
        computer: "mini".into(),
        project: "ember".into(),
        folder: "/w".into(),
        current: current.map(str::to_owned),
        entries,
    }
}

fn doc<'a>(path: &'a str, generation: u64, version: u32, dirty: bool, text: &'a str) -> DocView<'a> {
    DocView { path, generation, version, dirty, text }
}

fn backup_path(dir: &Path) -> PathBuf {
    dir.join(backup_file_name(&key()))
}

#[test]
fn fr_l31_hot_exit_the_backup_format_round_trips() {
    let written = file(
        vec![entry("/w/a.rs", "fn a() { 1 }\n", "fn a() {}\n"), BackupEntry { path: "/w/한글.md".into(), text: "새 글\n".into(), base: None }],
        Some("/w/a.rs"),
        NOW,
    );
    let bytes = encode(&written);
    assert_eq!(decode(&bytes), Ok(written.clone()));
    // The fields a reader of the file relies on.
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(value["written_at_ms"], NOW);
    assert_eq!(value["entries"][0]["path"], "/w/a.rs");
    assert_eq!(value["entries"][0]["text"], "fn a() { 1 }\n");
    // The disk text is kept as a mark, not as text.
    assert_eq!(value["entries"][0]["base"]["len"], 10);
    assert_eq!(value["entries"][0]["base"]["hash"].as_str().map(str::len), Some(16));
    assert!(!String::from_utf8(bytes).unwrap().contains("fn a() {}\\n"));
}

#[test]
fn fr_l31_hot_exit_the_base_mark_tells_texts_apart_and_is_stable() {
    assert_eq!(BaseMark::of("a\n"), BaseMark::of("a\n"));
    assert_ne!(BaseMark::of("a\n"), BaseMark::of("b\n"));
    assert_ne!(BaseMark::of(""), BaseMark::of("\n"));
    // FNV-1a 64 of the empty input is its offset basis: the mark does not depend on the build.
    assert_eq!(BaseMark::of(""), BaseMark { len: 0, hash: "cbf29ce484222325".into() });
    assert_eq!(BaseMark::of("a").hash, "af63dc4c8601ec8c");
}

#[test]
fn fr_l31_hot_exit_another_version_or_a_broken_file_is_not_read() {
    let mut newer = serde_json::to_value(file(vec![], None, NOW)).unwrap();
    newer["version"] = serde_json::json!(2);
    // A newer format may have other fields: the version is checked first.
    newer["entries"] = serde_json::json!("moved elsewhere");
    assert_eq!(decode(&serde_json::to_vec(&newer).unwrap()), Err(BackupError::Version(2)));
    assert!(matches!(decode(b"{\"entries\":[]}"), Err(BackupError::Corrupt(_))));
    assert!(matches!(decode(b"not json"), Err(BackupError::Corrupt(_))));
    assert!(matches!(decode(b"{\"version\":1}"), Err(BackupError::Corrupt(_))));
    // An entry without a base (the window never saw the file clean) reads.
    let bytes = br#"{"version":1,"written_at_ms":5,"computer":"c","project":"p","folder":"/f","entries":[{"path":"/f/a","text":"x"}]}"#;
    let read = decode(bytes).unwrap();
    assert_eq!(read.entries[0].base, None);
    assert_eq!(read.current, None);
}

#[test]
fn fr_l31_hot_exit_each_computer_project_and_folder_has_its_own_file_beside_the_prefs() {
    assert_eq!(backup_dir(Path::new("/data/ember")), PathBuf::from("/data/ember/editor/Backups"));
    let name = backup_file_name(&key());
    assert_eq!(name, backup_file_name(&key()));
    assert!(name.starts_with("ember-") && name.ends_with(".json"), "{name}");
    let others = [
        backup_file_name(&BackupKey::new("air", "ember", "/w")),
        backup_file_name(&BackupKey::new("mini", "core", "/w")),
        backup_file_name(&BackupKey::new("mini", "ember", "/w/sub")),
        // The parts do not run together: ("a", "b/c") is not ("a/b", "c").
        backup_file_name(&BackupKey::new("mini", "ember/w", "")),
    ];
    for (i, other) in others.iter().enumerate() {
        assert_ne!(*other, name, "{i}");
    }
    // A project name is free text: the file name is still one path component.
    let odd = backup_file_name(&BackupKey::new("mini", "../a b:c", "/w"));
    assert!(!odd.contains('/') && !odd.contains("..") && !odd.contains(' '), "{odd}");
}

#[test]
fn fr_l31_hot_exit_the_write_is_atomic_and_owner_only() {
    let dir = scratch();
    let path = dir.path().join("editor").join("Backups").join("x.json");
    write_atomic(&path, b"one").unwrap();
    write_atomic(&path, b"two").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"two");
    // The temporary file is renamed, not left behind.
    let names: Vec<String> = std::fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["x.json"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}

#[test]
fn fr_l31_hot_exit_an_edit_is_written_with_its_text_and_the_mark_of_the_disk_text() {
    let dir = scratch();
    let mut tracker = Tracker::new(key(), None);
    // Nothing is written for a window with no unsaved buffer.
    let seen = tracker.observe(&[doc("/w/a.rs", 1, 0, false, "fn a() {}\n")], Some("/w/a.rs"));
    assert!(!seen.changed);
    assert_eq!(tracker.flush(dir.path(), NOW).unwrap(), Flush::Unchanged);
    assert!(!backup_path(dir.path()).exists());

    // An edit: the buffer is dirty, a write is due.
    let before = tracker.seq();
    let seen = tracker.observe(&[doc("/w/a.rs", 1, 1, true, "fn a() { 1 }\n"), doc("/w/b.rs", 2, 0, false, "b\n")], Some("/w/a.rs"));
    assert!(seen.changed);
    assert!(seen.conflicts.is_empty());
    assert_ne!(tracker.seq(), before);
    assert_eq!(tracker.flush(dir.path(), NOW).unwrap(), Flush::Written(backup_path(dir.path())));
    let read = decode(&std::fs::read(backup_path(dir.path())).unwrap()).unwrap();
    assert_eq!(read, file(vec![entry("/w/a.rs", "fn a() { 1 }\n", "fn a() {}\n")], Some("/w/a.rs"), NOW));

    // The same state again: no change, no write.
    let seq = tracker.seq();
    let seen = tracker.observe(&[doc("/w/a.rs", 1, 1, true, "fn a() { 1 }\n"), doc("/w/b.rs", 2, 0, false, "b\n")], Some("/w/b.rs"));
    assert!(!seen.changed);
    assert_eq!(tracker.seq(), seq);
    assert_eq!(tracker.flush(dir.path(), NOW + 5).unwrap(), Flush::Unchanged);

    // Another edit and a second dirty file: both are in the file, in tab order, with the newest text.
    let seen = tracker.observe(&[doc("/w/a.rs", 1, 2, true, "fn a() { 12 }\n"), doc("/w/b.rs", 2, 1, true, "bb\n")], Some("/w/b.rs"));
    assert!(seen.changed);
    tracker.flush(dir.path(), NOW + 10).unwrap();
    let read = decode(&std::fs::read(backup_path(dir.path())).unwrap()).unwrap();
    assert_eq!(read, file(vec![entry("/w/a.rs", "fn a() { 12 }\n", "fn a() {}\n"), entry("/w/b.rs", "bb\n", "b\n")], Some("/w/b.rs"), NOW + 10));
    assert_eq!(BACKUP_DEBOUNCE_MS, 1000);
}

#[test]
fn fr_l31_hot_exit_a_buffer_never_seen_clean_has_no_base() {
    let mut tracker = Tracker::new(key(), None);
    tracker.observe(&[doc("/w/a.rs", 1, 3, true, "typed\n")], None);
    let snapshot = tracker.snapshot(NOW).unwrap();
    assert_eq!(snapshot.entries, [BackupEntry { path: "/w/a.rs".into(), text: "typed\n".into(), base: None }]);
    assert_eq!(snapshot.current, None);
}

#[test]
fn fr_l31_hot_exit_opening_the_window_restores_each_buffer_dirty_and_focuses_the_current_tab() {
    let loaded = file(vec![entry("/w/a.rs", "fn a() { 1 }\n", "fn a() {}\n"), entry("/w/b.rs", "bb\n", "b\n")], Some("/w/a.rs"), NOW);
    let mut tracker = Tracker::new(key(), Some(loaded.clone()));
    assert_eq!(
        tracker.take_restore_plan(),
        [
            Reopen::Restore { path: "/w/a.rs".into(), text: "fn a() { 1 }\n".into() },
            Reopen::Restore { path: "/w/b.rs".into(), text: "bb\n".into() },
            Reopen::Focus("/w/a.rs".into()),
        ]
    );
    // Once: a later render or a reconnect does not restore the backup again.
    assert!(tracker.take_restore_plan().is_empty());

    // Until the documents show the restored text the backup stays what it was.
    let seen = tracker.observe(&[], None);
    assert!(!seen.changed);
    assert_eq!(tracker.snapshot(NOW + 1).unwrap().entries, loaded.entries);
    tracker.opened("/w/a.rs", "fn a() {}\n");
    // The session opened the file (the disk text, clean) and has not put the buffer in yet.
    let seen = tracker.observe(&[doc("/w/a.rs", 1, 0, false, "fn a() {}\n")], Some("/w/a.rs"));
    assert!(!seen.changed);
    assert_eq!(tracker.snapshot(NOW + 1).unwrap().entries, loaded.entries);

    // Restored: the text is in, and the dirty mark may come a moment later.
    let seen = tracker.observe(&[doc("/w/a.rs", 2, 0, false, "fn a() { 1 }\n")], Some("/w/a.rs"));
    assert!(seen.conflicts.is_empty());
    assert_eq!(tracker.snapshot(NOW + 2).unwrap().entries.len(), 2);
    let seen = tracker.observe(&[doc("/w/a.rs", 2, 0, true, "fn a() { 1 }\n")], Some("/w/a.rs"));
    assert!(seen.conflicts.is_empty());
    // The base stays the disk text the edits started from.
    let entries = tracker.snapshot(NOW + 3).unwrap().entries;
    assert!(entries.contains(&entry("/w/a.rs", "fn a() { 1 }\n", "fn a() {}\n")));
    assert!(entries.contains(&entry("/w/b.rs", "bb\n", "b\n")));
}

#[test]
fn fr_l31_hot_exit_the_plan_focuses_the_last_tab_when_the_current_one_is_not_in_the_backup() {
    let loaded = file(vec![entry("/w/a.rs", "1\n", "0\n"), entry("/w/b.rs", "2\n", "0\n")], Some("/w/clean.rs"), NOW);
    let plan = Tracker::new(key(), Some(loaded)).take_restore_plan();
    assert_eq!(plan.last(), Some(&Reopen::Focus("/w/b.rs".into())));
    assert!(Tracker::new(key(), None).take_restore_plan().is_empty());
}

#[test]
fn fr_l31_hot_exit_a_file_changed_on_disk_since_the_backup_is_a_conflict() {
    let loaded = file(vec![entry("/w/a.rs", "mine\n", "base\n"), entry("/w/b.rs", "mine b\n", "base b\n")], None, NOW);
    let mut tracker = Tracker::new(key(), Some(loaded));
    tracker.take_restore_plan();
    // a.rs was changed by someone else while the window was closed; b.rs is as it was.
    tracker.opened("/w/a.rs", "theirs\n");
    tracker.opened("/w/b.rs", "base b\n");
    let seen = tracker.observe(&[doc("/w/a.rs", 2, 0, true, "mine\n"), doc("/w/b.rs", 4, 0, true, "mine b\n")], None);
    assert_eq!(seen.conflicts, ["/w/a.rs"]);
    // Reported once: the window raises the bar once.
    let seen = tracker.observe(&[doc("/w/a.rs", 2, 0, true, "mine\n"), doc("/w/b.rs", 4, 0, true, "mine b\n")], None);
    assert!(seen.conflicts.is_empty());
    // The buffer stays in the backup with its old base, so the conflict shows again after another close.
    assert!(tracker.snapshot(NOW).unwrap().entries.contains(&entry("/w/a.rs", "mine\n", "base\n")));
}

#[test]
fn fr_l31_hot_exit_a_file_that_now_equals_the_buffer_opens_clean_and_leaves_the_backup() {
    let dir = scratch();
    let loaded = file(vec![entry("/w/a.rs", "mine\n", "base\n")], None, NOW);
    write_atomic(&backup_path(dir.path()), &encode(&loaded)).unwrap();
    let mut tracker = Tracker::new(key(), Some(loaded));
    tracker.take_restore_plan();
    // The disk already has the buffer's text (saved from another window): restoring changes nothing.
    tracker.opened("/w/a.rs", "mine\n");
    let seen = tracker.observe(&[doc("/w/a.rs", 1, 0, false, "mine\n")], None);
    assert!(seen.changed);
    assert!(seen.conflicts.is_empty());
    assert_eq!(tracker.snapshot(NOW), None);
    assert_eq!(tracker.flush(dir.path(), NOW).unwrap(), Flush::Removed(backup_path(dir.path())));
    assert!(!backup_path(dir.path()).exists());
}

#[test]
fn fr_l31_hot_exit_a_file_that_cannot_be_opened_keeps_its_backup_and_is_reported_once() {
    let loaded = file(vec![entry("/w/a.rs", "mine\n", "base\n")], None, NOW);
    let mut tracker = Tracker::new(key(), Some(loaded.clone()));
    tracker.take_restore_plan();
    assert!(tracker.open_failed("/w/a.rs"));
    assert!(!tracker.open_failed("/w/a.rs"));
    // A file the backup does not know is not this module's to report.
    assert!(!tracker.open_failed("/w/other.rs"));
    tracker.observe(&[], None);
    assert_eq!(tracker.snapshot(NOW).unwrap().entries, loaded.entries);
    let text = restore_failed_notice("a.rs", "파일을 읽거나 쓸 권한이 없습니다");
    assert!(text.contains("a.rs") && text.contains("복원하지 못했습니다") && text.contains("권한"), "{text}");
}

#[test]
fn fr_l31_hot_exit_a_save_removes_the_file_from_the_backup() {
    let dir = scratch();
    let mut tracker = Tracker::new(key(), None);
    tracker.observe(&[doc("/w/a.rs", 1, 0, false, "a\n"), doc("/w/b.rs", 2, 0, false, "b\n")], None);
    tracker.observe(&[doc("/w/a.rs", 1, 1, true, "a1\n"), doc("/w/b.rs", 2, 1, true, "b1\n")], None);
    tracker.flush(dir.path(), NOW).unwrap();
    // a.rs is saved: same text, no longer dirty.
    let seen = tracker.observe(&[doc("/w/a.rs", 1, 1, false, "a1\n"), doc("/w/b.rs", 2, 1, true, "b1\n")], None);
    assert!(seen.changed);
    assert_eq!(tracker.flush(dir.path(), NOW + 1).unwrap(), Flush::Written(backup_path(dir.path())));
    let read = decode(&std::fs::read(backup_path(dir.path())).unwrap()).unwrap();
    assert_eq!(read.entries, [entry("/w/b.rs", "b1\n", "b\n")]);
    // The saved text is the base of the next edit.
    tracker.observe(&[doc("/w/a.rs", 1, 2, true, "a12\n"), doc("/w/b.rs", 2, 1, true, "b1\n")], None);
    assert!(tracker.snapshot(NOW).unwrap().entries.contains(&entry("/w/a.rs", "a12\n", "a1\n")));
    // Both saved: the file goes.
    tracker.observe(&[doc("/w/a.rs", 1, 2, false, "a12\n"), doc("/w/b.rs", 2, 1, false, "b1\n")], None);
    assert_eq!(tracker.flush(dir.path(), NOW + 2).unwrap(), Flush::Removed(backup_path(dir.path())));
    assert!(!backup_path(dir.path()).exists());
    // Removing a file that is not there is not an error.
    tracker.observe(&[doc("/w/a.rs", 1, 3, true, "x\n")], None);
    tracker.observe(&[doc("/w/a.rs", 1, 3, false, "x\n")], None);
    assert_eq!(tracker.flush(dir.path(), NOW + 3).unwrap(), Flush::Removed(backup_path(dir.path())));
}

#[test]
fn fr_l31_hot_exit_a_revert_removes_the_file_from_the_backup() {
    let dir = scratch();
    let mut tracker = Tracker::new(key(), None);
    tracker.observe(&[doc("/w/a.rs", 1, 0, false, "disk\n")], None);
    tracker.observe(&[doc("/w/a.rs", 1, 4, true, "edited\n")], None);
    tracker.flush(dir.path(), NOW).unwrap();
    assert!(backup_path(dir.path()).exists());
    // 디스크 버전으로 되돌리기: the disk text in a new node, clean.
    let seen = tracker.observe(&[doc("/w/a.rs", 2, 0, false, "disk\n")], None);
    assert!(seen.changed);
    assert_eq!(tracker.flush(dir.path(), NOW + 1).unwrap(), Flush::Removed(backup_path(dir.path())));
    assert!(!backup_path(dir.path()).exists());
}

#[test]
fn fr_l31_hot_exit_a_restored_buffer_that_is_reverted_leaves_the_backup() {
    let loaded = file(vec![entry("/w/a.rs", "mine\n", "base\n")], None, NOW);
    let mut tracker = Tracker::new(key(), Some(loaded));
    tracker.take_restore_plan();
    tracker.opened("/w/a.rs", "base\n");
    tracker.observe(&[doc("/w/a.rs", 2, 0, true, "mine\n")], None);
    assert!(tracker.snapshot(NOW).is_some());
    let seen = tracker.observe(&[doc("/w/a.rs", 3, 0, false, "base\n")], None);
    assert!(seen.changed);
    assert_eq!(tracker.snapshot(NOW), None);
}

#[test]
fn fr_l31_hot_exit_closing_a_tab_without_saving_removes_its_backup() {
    let mut tracker = Tracker::new(key(), None);
    tracker.observe(&[doc("/w/a.rs", 1, 0, false, "a\n"), doc("/w/b.rs", 2, 0, false, "b\n")], None);
    tracker.observe(&[doc("/w/a.rs", 1, 1, true, "a1\n"), doc("/w/b.rs", 2, 1, true, "b1\n")], None);
    // 저장 안 함 on a.rs: its document is closed.
    let seen = tracker.observe(&[doc("/w/b.rs", 2, 1, true, "b1\n")], Some("/w/b.rs"));
    assert!(seen.changed);
    assert_eq!(tracker.snapshot(NOW).unwrap().entries, [entry("/w/b.rs", "b1\n", "b\n")]);
}

#[test]
fn fr_l31_hot_exit_a_failed_write_is_tried_again() {
    let dir = scratch();
    // The backup folder cannot be made: a file is in its place.
    let blocked = dir.path().join("blocked");
    std::fs::write(&blocked, b"").unwrap();
    let mut tracker = Tracker::new(key(), None);
    tracker.observe(&[doc("/w/a.rs", 1, 1, true, "a1\n")], None);
    assert!(tracker.flush(&blocked.join("Backups"), NOW).is_err());
    // Still due: the next flush writes it.
    assert_eq!(tracker.flush(dir.path(), NOW).unwrap(), Flush::Written(backup_path(dir.path())));
}

#[test]
fn fr_l31_hot_exit_a_backup_older_than_the_limit_is_dropped_at_open() {
    assert!(!expired(NOW, NOW, 30));
    assert!(!expired(NOW - 30 * DAY_MS, NOW, 30));
    assert!(expired(NOW - 30 * DAY_MS - 1, NOW, 30));
    // A clock that went back does not expire anything.
    assert!(!expired(NOW + DAY_MS, NOW, 30));

    let dir = scratch();
    let path = backup_path(dir.path());
    write_atomic(&path, &encode(&file(vec![entry("/w/a.rs", "mine\n", "base\n")], None, NOW - 31 * DAY_MS))).unwrap();
    assert_eq!(load(&path, &key(), NOW, 30), Loaded::Expired);
    assert!(!path.exists());
    assert_eq!(load(&path, &key(), NOW, 30), Loaded::None);

    // A fresh one is read.
    let fresh = file(vec![entry("/w/a.rs", "mine\n", "base\n")], None, NOW - 29 * DAY_MS);
    write_atomic(&path, &encode(&fresh)).unwrap();
    assert_eq!(load(&path, &key(), NOW, 30), Loaded::Restored(fresh));
    assert!(path.exists());
}

#[test]
fn fr_l31_hot_exit_old_backups_of_other_projects_are_pruned_too() {
    let dir = scratch();
    let old = dir.path().join("old-0000000000000001.json");
    let fresh = dir.path().join("fresh-0000000000000002.json");
    let broken = dir.path().join("broken-0000000000000003.json");
    let other = dir.path().join("notes.txt");
    write_atomic(&old, &encode(&file(vec![entry("/w/a.rs", "1\n", "0\n")], None, NOW - 40 * DAY_MS))).unwrap();
    write_atomic(&fresh, &encode(&file(vec![entry("/w/a.rs", "1\n", "0\n")], None, NOW - DAY_MS))).unwrap();
    std::fs::write(&broken, b"{").unwrap();
    std::fs::write(&other, b"old").unwrap();
    assert_eq!(prune_dir(dir.path(), NOW, 30), [old.clone()]);
    assert!(!old.exists() && fresh.exists() && broken.exists() && other.exists());
    // A folder that is not there is nothing to prune.
    assert!(prune_dir(&dir.path().join("none"), NOW, 30).is_empty());
}

#[test]
fn fr_l31_hot_exit_a_corrupt_backup_is_kept_aside_and_reported_without_stopping_the_window() {
    let dir = scratch();
    let path = backup_path(dir.path());
    std::fs::write(&path, b"{\"version\":1,\"entries\":").unwrap();
    let Loaded::Unreadable { reason, kept_as } = load(&path, &key(), NOW, 30) else { panic!("a broken file is reported") };
    assert!(!reason.is_empty());
    // Moved aside, so the next write does not replace what may still be recovered by hand.
    let aside = PathBuf::from(format!("{}.corrupt", path.display()));
    assert_eq!(kept_as, Some(aside.clone()));
    assert!(!path.exists());
    assert_eq!(std::fs::read(&aside).unwrap(), b"{\"version\":1,\"entries\":");
    // The window opens as one without a backup.
    assert_eq!(load(&path, &key(), NOW, 30), Loaded::None);
    assert!(Tracker::new(key(), None).take_restore_plan().is_empty());

    // Another format version is treated the same way.
    std::fs::write(&path, br#"{"version":9}"#).unwrap();
    assert!(matches!(load(&path, &key(), NOW, 30), Loaded::Unreadable { .. }));

    let text = unreadable_notice(Some(&aside));
    assert!(text.contains("복원하지 않았습니다") && text.contains(".corrupt"), "{text}");
    assert!(unreadable_notice(None).ends_with("복원하지 않았습니다"));
}

#[test]
fn fr_l31_hot_exit_a_backup_of_another_window_under_the_same_name_is_not_restored() {
    let dir = scratch();
    let path = backup_path(dir.path());
    let mut other = file(vec![entry("/x/a.rs", "1\n", "0\n")], None, NOW);
    other.folder = "/x".into();
    write_atomic(&path, &encode(&other)).unwrap();
    assert_eq!(load(&path, &key(), NOW, 30), Loaded::None);
}

#[test]
fn fr_l31_hot_exit_the_limit_is_kept_per_device_and_reads_30_days_from_older_files() {
    assert_eq!(DEFAULT_BACKUP_DAYS, 30);
    assert_eq!(Prefs::default().hot_exit_backup_days, 30);
    let dir = scratch();
    let path = dir.path().join("prefs.json");
    std::fs::write(&path, br#"{"format":1}"#).unwrap();
    assert_eq!(prefs::load(&path).hot_exit_backup_days, 30);
    let p = Prefs { hot_exit_backup_days: 7, ..Prefs::default() };
    prefs::save(&path, &p).unwrap();
    assert_eq!(prefs::load(&path).hot_exit_backup_days, 7);
}
