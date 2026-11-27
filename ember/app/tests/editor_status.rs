//! FR-L31.7 (#453): the status bar's EOL, encoding and language items of the Ember 에디터 window and
//! their pickers. Pure parts only, no renderer.

use ember_app::ui::editor_status::{
    encoding_text, initial_selection, resolve_selection, FOLLOW_CURRENT,
    is_picker, picker_items, reopen_failure_text, reopen_needs_confirmation, status_texts,
    StatusTexts, AUTO_DETECT, CANCEL, DISCARD_AND_REOPEN, REOPEN_WITH_ENCODING, SAVE_WITH_ENCODING, UNKNOWN,
};
use ember_app::ui::palette::{lists_files, placeholder, rank, search_label, Mode, Payload};
use ember_app::ui::keymap::Keymap;
use ember_editor::{DocumentFormat, Encoding, EncodingMode, Eol, LanguageInfo};

fn fmt(eol: Eol, encoding: Encoding, id: &str, name: &str) -> DocumentFormat {
    DocumentFormat { eol, encoding, language_id: id.into(), language_name: name.into() }
}

fn langs() -> Vec<LanguageInfo> {
    vec![
        LanguageInfo { id: "json".into(), name: "JSON".into() },
        LanguageInfo { id: "markdown".into(), name: "Markdown".into() },
        LanguageInfo { id: "plaintext".into(), name: "Plain Text".into() },
    ]
}

#[test]
fn fr_l31_7_status_bar_texts_follow_the_document() {
    let f = fmt(Eol::Crlf, Encoding::Utf16Le, "json", "JSON");
    assert_eq!(
        status_texts(Some(&f)),
        StatusTexts { eol: "CRLF".into(), encoding: "UTF-16 LE".into(), language: "JSON".into() }
    );
    let f = fmt(Eol::Lf, Encoding::Utf8Bom, "zig", "");
    let t = status_texts(Some(&f));
    assert_eq!((t.eol.as_str(), t.encoding.as_str(), t.language.as_str()), ("LF", "UTF-8 with BOM", "zig"));
    // Before the session reported the format: nothing is claimed.
    let t = status_texts(None);
    assert_eq!((t.encoding.as_str(), t.language.as_str()), (UNKNOWN, UNKNOWN));
    assert_eq!(UNKNOWN, "알 수 없음");
    assert_eq!(encoding_text(None), "알 수 없음");
    assert_eq!(encoding_text(Some(Encoding::Utf16Be)), "UTF-16 BE");
    assert_eq!(encoding_text(Some(Encoding::Utf8)), "UTF-8");
}

#[test]
fn fr_l31_7_eol_picker_offers_lf_and_crlf_on_the_current_one() {
    let f = fmt(Eol::Crlf, Encoding::Utf8, "json", "JSON");
    let items = picker_items(Mode::Eol, Some(&f), &[]);
    let titles: Vec<&str> = items.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, ["LF", "CRLF"]);
    assert_eq!(items[0].payload, Payload::SetEol(Eol::Lf));
    assert_eq!(items[1].payload, Payload::SetEol(Eol::Crlf));
    assert_eq!(items[1].hint, "현재");
    assert_eq!(initial_selection(Mode::Eol, Some(&f), &[]), 1);
}

#[test]
fn fr_l31_7_encoding_picker_has_two_steps_and_the_four_encodings() {
    let f = fmt(Eol::Lf, Encoding::Utf8Bom, "json", "JSON");
    let actions = picker_items(Mode::EncodingAction, Some(&f), &[]);
    assert_eq!(actions.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), [REOPEN_WITH_ENCODING, SAVE_WITH_ENCODING]);
    assert_eq!((REOPEN_WITH_ENCODING, SAVE_WITH_ENCODING), ("인코딩으로 다시 열기", "인코딩으로 저장"));
    assert_eq!(actions[0].payload, Payload::EncodingAction(EncodingMode::Reopen));
    assert_eq!(actions[1].payload, Payload::EncodingAction(EncodingMode::Save));
    for mode in [EncodingMode::Reopen, EncodingMode::Save] {
        let items = picker_items(Mode::Encoding(mode), Some(&f), &[]);
        let titles: Vec<&str> = items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["UTF-8", "UTF-8 with BOM", "UTF-16 LE", "UTF-16 BE"]);
        assert_eq!(items[2].payload, Payload::SetEncoding { mode, encoding: Encoding::Utf16Le });
        assert_eq!(items[0].hint, "utf8");
        assert_eq!(items[1].hint, "utf8bom - 현재");
        assert_eq!(initial_selection(Mode::Encoding(mode), Some(&f), &[]), 1);
    }
    assert_ne!(placeholder(Mode::Encoding(EncodingMode::Reopen), Keymap::VsCode), placeholder(Mode::Encoding(EncodingMode::Save), Keymap::VsCode));
}

#[test]
fn fr_l31_7_reopen_over_unsaved_edits_asks_first_with_cancel_as_default() {
    let f = fmt(Eol::Lf, Encoding::Utf8, "json", "JSON");
    assert!(reopen_needs_confirmation(true, Some(&f), Encoding::Utf16Le));
    assert!(!reopen_needs_confirmation(false, Some(&f), Encoding::Utf16Le), "clean: nothing to lose");
    assert!(!reopen_needs_confirmation(true, Some(&f), Encoding::Utf8), "same encoding: a reopen does nothing");
    let mode = Mode::ConfirmReopen(Encoding::Utf16Le);
    let items = picker_items(mode, Some(&f), &[]);
    assert_eq!(items.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), [DISCARD_AND_REOPEN, CANCEL]);
    assert_eq!(items[0].payload, Payload::SetEncoding { mode: EncodingMode::Reopen, encoding: Encoding::Utf16Le });
    assert_eq!(items[1].payload, Payload::Cancel);
    assert_eq!(initial_selection(mode, Some(&f), &[]), 1, "취소 is highlighted");
}

#[test]
fn fr_l31_7_language_picker_lists_auto_detect_then_names_with_ids() {
    let f = fmt(Eol::Lf, Encoding::Utf8, "markdown", "Markdown");
    let items = picker_items(Mode::Language, Some(&f), &langs());
    assert_eq!(items.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), [AUTO_DETECT, "JSON", "Markdown", "Plain Text"]);
    assert_eq!(items[0].payload, Payload::SetLanguage(None));
    assert_eq!(items[1].payload, Payload::SetLanguage(Some("json".into())));
    assert_eq!(items[1].hint, "(json)");
    assert_eq!(items[2].hint, "(markdown) - 현재");
    assert_eq!(initial_selection(Mode::Language, Some(&f), &langs()), 2);
    // Typing filters by name, like every palette list.
    let ranked = rank("mark", &items);
    assert_eq!(items[ranked[0].index].title, "Markdown");
}

#[test]
fn fr_l31_7_pickers_are_palette_modes_without_the_file_index() {
    for mode in [Mode::Eol, Mode::EncodingAction, Mode::Encoding(EncodingMode::Save), Mode::ConfirmReopen(Encoding::Utf8), Mode::Language] {
        assert!(is_picker(mode));
        assert!(!lists_files(mode));
        assert!(!placeholder(mode, Keymap::JetBrains).is_empty());
        assert!(!search_label(mode).is_empty());
    }
    for mode in [Mode::Commands, Mode::Files, Mode::Everything, Mode::GotoLine, Mode::Recent] {
        assert!(!is_picker(mode));
    }
    assert!(lists_files(Mode::Files) && lists_files(Mode::Recent) && lists_files(Mode::Everything));
    assert_eq!(placeholder(Mode::Eol, Keymap::VsCode), "줄 끝 시퀀스 선택");
    assert_eq!(placeholder(Mode::Language, Keymap::VsCode), "언어 모드 선택");
}

#[test]
fn fr_l31_7_reopen_failure_says_why_in_korean() {
    let t = reopen_failure_text("a.txt", Some(Encoding::Utf16Le), "file is not valid UTF-16 LE");
    assert_eq!(t, "a.txt을(를) UTF-16 LE(으)로 다시 열지 못했습니다: 파일 내용이 이 인코딩에 맞지 않습니다");
    let t = reopen_failure_text("a.txt", None, "EntryNotFound (FileSystemError)");
    assert!(t.starts_with("a.txt을(를) 다시 열지 못했습니다: 파일이 없습니다"), "{t}");
}

// The picker selects and tags the current language even far down a long list.
#[test]
fn fr_l31_7_language_picker_opens_on_the_current_language() {
    // Names that sort before JSON put it below the first page.
    let mut languages: Vec<LanguageInfo> =
        (0..80).map(|i| LanguageInfo { id: format!("lang{i:02}"), name: format!("Aa {i:02}") }).collect();
    languages.push(LanguageInfo { id: "json".into(), name: "JSON".into() });
    languages.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    let f = fmt(Eol::Lf, Encoding::Utf8, "json", "JSON");
    let items = picker_items(Mode::Language, Some(&f), &languages);
    let sel = initial_selection(Mode::Language, Some(&f), &languages);
    assert_eq!(items[sel].title, "JSON");
    assert_eq!(items[sel].hint, "(json) - 현재");
    assert_eq!(items.iter().filter(|i| i.hint.ends_with("현재")).count(), 1);
    assert_eq!(sel, 81);
}

// The language list can arrive after the sheet opened: until the person moves, the highlight follows
// the current language in the rows as they are now; after a move it stays where they put it.
#[test]
fn fr_l31_7_picker_highlight_follows_the_current_entry_until_moved() {
    let f = fmt(Eol::Lf, Encoding::Utf8, "json", "JSON");
    let early = picker_items(Mode::Language, Some(&f), &[]);
    assert_eq!(resolve_selection(FOLLOW_CURRENT, Mode::Language, Some(&f), &[], early.len()), 0, "only 자동 감지 so far");
    let mut later: Vec<LanguageInfo> =
        (0..40).map(|i| LanguageInfo { id: format!("x{i:02}"), name: format!("Handlebars {i:02}") }).collect();
    later.push(LanguageInfo { id: "json".into(), name: "JSON".into() });
    later.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    let items = picker_items(Mode::Language, Some(&f), &later);
    let row = resolve_selection(FOLLOW_CURRENT, Mode::Language, Some(&f), &later, items.len());
    assert_eq!(items[row].title, "JSON");
    // A row the person chose is kept, clamped to the list.
    assert_eq!(resolve_selection(3, Mode::Language, Some(&f), &later, items.len()), 3);
    assert_eq!(resolve_selection(500, Mode::Language, Some(&f), &later, items.len()), items.len() - 1);
    assert_ne!(FOLLOW_CURRENT, 0);
}

// The encoding picker also opens on, and tags, the current encoding, in both steps' lists.
#[test]
fn fr_l31_7_encoding_picker_opens_on_the_current_encoding() {
    for (encoding, row) in [(Encoding::Utf8, 0), (Encoding::Utf8Bom, 1), (Encoding::Utf16Le, 2), (Encoding::Utf16Be, 3)] {
        let f = fmt(Eol::Lf, encoding, "json", "JSON");
        for mode in [EncodingMode::Reopen, EncodingMode::Save] {
            let items = picker_items(Mode::Encoding(mode), Some(&f), &[]);
            let sel = initial_selection(Mode::Encoding(mode), Some(&f), &[]);
            assert_eq!(sel, row);
            assert!(items[sel].hint.ends_with("- 현재"), "{:?}", items[sel].hint);
            assert_eq!(items.iter().filter(|i| i.hint.ends_with("현재")).count(), 1);
        }
    }
}

// #302: the indentation pick (Code-OSS `showIndentationPicker`, `ChangeIndentationSizeAction`).

#[test]
fn fr_l31_indentation_pick_rows_follow_code_oss_in_korean() {
    use ember_app::ui::editor_status::{indentation_initial_selection, indentation_items, indentation_text, is_indentation_pick};
    use ember_app::ui::palette::IndentationSizeFor;
    use ember_editor::{Indentation, IndentationAction};

    let current = Indentation::new(4, true);
    let items = indentation_items(Mode::Indentation, current, 4);
    let titles: Vec<&str> = items.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "공백을 사용한 들여쓰기",
            "탭을 사용한 들여쓰기",
            "탭 표시 크기 변경",
            "콘텐츠에서 들여쓰기 감지",
            "들여쓰기를 공백으로 변환",
            "들여쓰기를 탭으로 변환",
            "후행 공백 자르기",
        ]
    );
    assert_eq!(items[0].payload, Payload::IndentationSize(IndentationSizeFor::Spaces));
    assert_eq!(items[3].payload, Payload::SetIndentation(IndentationAction::Detect));
    assert_eq!(items[5].payload, Payload::SetIndentation(IndentationAction::Convert { to_spaces: false }));

    // The size list: 1 to 8, the settings' size and the current one marked as Code-OSS marks them.
    let current = Indentation { tab_size: 6, indent_size: 6, insert_spaces: true };
    let sizes = indentation_items(Mode::IndentationSize(IndentationSizeFor::Tabs), current, 2);
    assert_eq!(sizes.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), ["1", "2", "3", "4", "5", "6", "7", "8"]);
    assert_eq!(sizes[1].hint, "기본 탭 크기");
    assert_eq!(sizes[5].hint, "현재 탭 크기");
    assert_eq!(sizes[0].hint, "");
    let same = indentation_items(Mode::IndentationSize(IndentationSizeFor::Spaces), Indentation::new(2, true), 2);
    assert_eq!(same[1].hint, "구성된 탭 크기");
    assert_eq!(same[1].payload, Payload::SetIndentation(IndentationAction::IndentUsing { insert_spaces: true, size: 2 }));
    let display = indentation_items(Mode::IndentationSize(IndentationSizeFor::Display), current, 4);
    assert_eq!(display[7].payload, Payload::SetIndentation(IndentationAction::DisplaySize(8)));

    // The size list opens on the current tab size; the action list on its first row.
    assert_eq!(indentation_initial_selection(Mode::IndentationSize(IndentationSizeFor::Spaces), current), 5);
    assert_eq!(indentation_initial_selection(Mode::IndentationSize(IndentationSizeFor::Spaces), Indentation::new(16, true)), 7);
    assert_eq!(indentation_initial_selection(Mode::Indentation, current), 0);

    for mode in [Mode::Indentation, Mode::IndentationSize(IndentationSizeFor::Display)] {
        assert!(is_picker(mode) && is_indentation_pick(mode) && !lists_files(mode));
        assert!(!placeholder(mode, Keymap::VsCode).is_empty() && !search_label(mode).is_empty());
    }
    assert_eq!(placeholder(Mode::Indentation, Keymap::VsCode), "작업 선택");
    assert_eq!(placeholder(Mode::IndentationSize(IndentationSizeFor::Spaces), Keymap::VsCode), "현재 파일의 탭 크기 선택");

    // The status text: spaces, tabs, and spaces with a tab of another width.
    assert_eq!(indentation_text(Indentation::new(4, true)), "4칸");
    assert_eq!(indentation_text(Indentation::new(2, false)), "탭 크기: 2");
    assert_eq!(indentation_text(Indentation { tab_size: 8, indent_size: 2, insert_spaces: true }), "2칸 (탭 크기: 8)");
}
