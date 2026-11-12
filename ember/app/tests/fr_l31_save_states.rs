//! FR-L31.5: what the Ember 에디터 window shows about a file and the connection, and auto-save. The pure
//! parts, no renderer.

use ember_app::prefs::{self, Prefs};
use ember_app::ui::editor_states::{
    compare_rows, connection_banner, doc_bar, is_deleted, manual_save_gate, reopen_plan, should_auto_save, tab_label, tab_label_style,
    visible_compare_rows, AutoSave, AutoSaveTimer, BarTone, CompareKind, CompareView, DocAction, Reopen, SaveTrigger, AUTO_SAVE_DELAY_MS,
    CONFLICT_SAVE_NOTICE,
};
use ember_editor::ExternalChange;

#[test]
fn fr_l31_save_states_a_failed_save_names_the_file_and_reason_and_offers_retry() {
    let bar = doc_bar("main.rs", None, Some("EACCES: permission denied, open '/w/main.rs'")).expect("a bar");
    assert_eq!(bar.tone, BarTone::Error);
    assert_eq!(bar.text, "main.rs을(를) 저장하지 못했습니다: 파일을 읽거나 쓸 권한이 없습니다");
    assert_eq!(bar.actions, vec![DocAction::Retry, DocAction::Dismiss]);
    assert_eq!(DocAction::Retry.label(), "다시 시도");
    // The server's own words (paths, English) are not shown.
    assert!(!bar.text.contains("EACCES"));
}

#[test]
fn fr_l31_save_states_a_dirty_buffer_changed_on_disk_offers_compare_overwrite_and_revert() {
    let bar = doc_bar("a.json", Some(ExternalChange::ChangedWhileDirty), None).expect("a bar");
    assert_eq!(bar.tone, BarTone::Warning);
    assert!(bar.text.starts_with("a.json 파일이 디스크에서 바뀌었습니다."), "{}", bar.text);
    assert_eq!(bar.actions, vec![DocAction::Compare, DocAction::Overwrite, DocAction::Revert]);
    let labels: Vec<_> = bar.actions.iter().map(|a| a.label()).collect();
    assert_eq!(labels, ["비교", "덮어쓰기", "디스크 버전으로 되돌리기"]);
    // A failed save under a conflict still says why first, and keeps the way out.
    let both = doc_bar("a.json", Some(ExternalChange::ChangedWhileDirty), Some("timed out")).unwrap();
    assert_eq!(both.tone, BarTone::Error);
    assert_eq!(both.actions, vec![DocAction::Retry, DocAction::Compare, DocAction::Revert, DocAction::Dismiss]);
    // Nothing to say: no bar. A deleted file shows only the tab mark.
    assert_eq!(doc_bar("a.json", None, None), None);
    assert_eq!(doc_bar("a.json", Some(ExternalChange::Deleted), None), None);
}

#[test]
fn fr_l31_save_states_a_manual_save_waits_for_the_conflict_choice() {
    assert_eq!(manual_save_gate(None), Ok(()));
    assert_eq!(manual_save_gate(Some(ExternalChange::Deleted)), Ok(()), "saving a deleted file writes it again");
    assert_eq!(manual_save_gate(Some(ExternalChange::ChangedWhileDirty)), Err(CONFLICT_SAVE_NOTICE));
}

#[test]
fn fr_l31_save_states_a_deleted_file_tab_is_struck_through_and_says_so() {
    assert!(is_deleted(Some(ExternalChange::Deleted)));
    assert!(!is_deleted(Some(ExternalChange::ChangedWhileDirty)));
    assert!(!is_deleted(None));
    assert_eq!(tab_label("main.rs", true), "main.rs (삭제됨)");
    assert_eq!(tab_label("main.rs", false), "main.rs");
    assert_eq!(tab_label_style(true), "text-decoration:line-through;");
    assert_eq!(tab_label_style(false), "");
}

#[test]
fn fr_l31_save_states_the_connection_banner_offers_reconnect_until_it_runs() {
    let (text, button) = connection_banner(false);
    assert_eq!(text, "서버와의 연결이 끊어졌습니다. 저장하지 않은 변경 사항은 이 창에 남아 있습니다");
    assert_eq!(button, Some("다시 연결"));
    let (text, button) = connection_banner(true);
    assert_eq!(text, "서버에 다시 연결하는 중입니다");
    assert_eq!(button, None);
}

#[test]
fn fr_l31_save_states_after_a_reconnect_every_tab_is_put_back_with_its_buffer() {
    let tabs = vec![
        ("/w/a.rs".to_string(), Some("fn a() {}\n".to_string())),
        ("/w/b.rs".to_string(), None),
        ("/w/c.rs".to_string(), Some("typed while lost\n".to_string())),
    ];
    assert_eq!(
        reopen_plan(&tabs, Some("/w/c.rs")),
        vec![
            Reopen::Restore { path: "/w/a.rs".into(), text: "fn a() {}\n".into() },
            Reopen::Open("/w/b.rs".into()),
            Reopen::Restore { path: "/w/c.rs".into(), text: "typed while lost\n".into() },
            Reopen::Focus("/w/c.rs".into()),
        ]
    );
    assert_eq!(reopen_plan(&[], None), vec![]);
    // A current path that is not a tab is not focused.
    assert_eq!(reopen_plan(&tabs[1..2], Some("/w/x.rs")), vec![Reopen::Open("/w/b.rs".into())]);
}

#[test]
fn fr_l31_save_states_auto_save_follows_code_oss_files_auto_save() {
    assert_eq!(AutoSave::default(), AutoSave::Off);
    assert_eq!(AUTO_SAVE_DELAY_MS, 1000);
    let settings: Vec<_> = AutoSave::ALL.iter().map(|m| m.setting()).collect();
    assert_eq!(settings, ["off", "afterDelay", "onFocusChange"]);
    let labels: Vec<_> = AutoSave::ALL.iter().map(|m| m.label()).collect();
    assert_eq!(labels, ["끄기", "입력을 멈추고 1초 뒤", "다른 탭으로 옮길 때"]);

    use SaveTrigger::{Delay, FocusChange};
    assert!(!should_auto_save(AutoSave::Off, Delay, true, None));
    assert!(!should_auto_save(AutoSave::Off, FocusChange, true, None));
    assert!(should_auto_save(AutoSave::AfterDelay, Delay, true, None));
    assert!(!should_auto_save(AutoSave::AfterDelay, FocusChange, true, None));
    assert!(should_auto_save(AutoSave::OnFocusChange, FocusChange, true, None));
    assert!(!should_auto_save(AutoSave::OnFocusChange, Delay, true, None));
    // Clean, in conflict or deleted: not saved by auto-save.
    assert!(!should_auto_save(AutoSave::AfterDelay, Delay, false, None));
    assert!(!should_auto_save(AutoSave::AfterDelay, Delay, true, Some(ExternalChange::ChangedWhileDirty)));
    assert!(!should_auto_save(AutoSave::AfterDelay, Delay, true, Some(ExternalChange::Deleted)));
}

#[test]
fn fr_l31_save_states_after_delay_saves_only_after_the_last_edit() {
    let mut timer = AutoSaveTimer::default();
    let first = timer.edited("/w/a.rs");
    let second = timer.edited("/w/a.rs");
    let other = timer.edited("/w/b.rs");
    // The wait of the first edit runs out after the second edit: no save yet.
    assert!(!timer.fire("/w/a.rs", first));
    assert!(timer.fire("/w/a.rs", second));
    // Each wait saves once.
    assert!(!timer.fire("/w/a.rs", second));
    timer.forget("/w/b.rs");
    assert!(!timer.fire("/w/b.rs", other));
}

#[test]
fn fr_l31_save_states_auto_save_is_kept_per_device_and_reads_off_from_older_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prefs.json");
    std::fs::write(&path, br#"{"format":1}"#).unwrap();
    assert_eq!(prefs::load(&path).auto_save, AutoSave::Off);
    let p = Prefs { auto_save: AutoSave::AfterDelay, ..Prefs::default() };
    prefs::save(&path, &p).unwrap();
    assert_eq!(prefs::load(&path).auto_save, AutoSave::AfterDelay);
    assert!(std::fs::read_to_string(&path).unwrap().contains("\"afterDelay\""));
}

#[test]
fn fr_l31_save_states_compare_pairs_the_disk_version_with_the_buffer() {
    let rows = compare_rows("a\nb\nc\nd\n", "a\nB\nc\nd\ne\n");
    let kinds: Vec<_> = rows.iter().map(|r| (r.kind, r.disk_line, r.mine_line, r.text.as_str())).collect();
    assert_eq!(
        kinds,
        vec![
            (CompareKind::Same, Some(1), Some(1), "a"),
            (CompareKind::Mine, None, Some(2), "B"),
            (CompareKind::Disk, Some(2), None, "b"),
            (CompareKind::Same, Some(3), Some(3), "c"),
            (CompareKind::Same, Some(4), Some(4), "d"),
            (CompareKind::Mine, None, Some(5), "e"),
        ]
    );
    // CRLF on disk and `\n` in the buffer are the same lines.
    assert!(compare_rows("x\r\ny\r\n", "x\ny\n").iter().all(|r| r.kind == CompareKind::Same));
    assert!(compare_rows("", "").is_empty());
}

#[test]
fn fr_l31_save_states_a_large_compare_does_not_build_a_large_matrix() {
    let disk: String = (0..1000).map(|i| format!("d{i}\n")).collect();
    let mine: String = (0..1000).map(|i| format!("m{i}\n")).collect();
    let rows = compare_rows(&disk, &mine);
    assert_eq!(rows.len(), 2000);
    assert!(rows[..1000].iter().all(|r| r.kind == CompareKind::Disk));
    assert!(rows[1000..].iter().all(|r| r.kind == CompareKind::Mine));
}

#[test]
fn fr_l31_save_states_compare_shows_changes_with_three_lines_around() {
    let disk: String = (1..=20).map(|i| format!("{i}\n")).collect();
    let mine = disk.replace("10\n", "ten\n");
    let rows = compare_rows(&disk, &mine);
    let shown = visible_compare_rows(&rows, 3);
    // A gap, lines 7 to 9, the change (two rows), lines 11 to 13, a gap.
    assert_eq!(shown.len(), 1 + 3 + 2 + 3 + 1);
    assert!(shown[0].is_none() && shown[shown.len() - 1].is_none());
    assert_eq!(shown[1].unwrap().text, "7");
    // No change: nothing to draw.
    assert!(visible_compare_rows(&compare_rows("a\n", "a\n"), 3).is_empty());
}

#[test]
fn fr_l31_save_states_the_compare_view_covers_only_its_tab() {
    let view = CompareView::Loading { path: "/w/a.rs".into() };
    assert!(view.visible(Some("/w/a.rs")));
    assert!(!view.visible(Some("/w/b.rs")));
    assert!(!view.visible(None));
}
