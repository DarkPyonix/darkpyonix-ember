# TOOLKIT-WORKAROUNDS.md: Ember workarounds for toolkit gaps (#673)

Status: **marker pass done on 2026-11-15, not compiled.** The table is read from the source of
`ember/app/` on branch `feat/673-marker-pass`. Line numbers are that branch's. No row here was
checked on a display. Branch `feat/673-drop-in-pin` then removes the base screens' workarounds
whose fix is in the pin; those rows are in their own table and name functions, not lines.
Branch `feat/673-editor-in-pin` does the same for the editor window, in a table of its own.

Korean follows the English section with the same content.

## English

### Rule

Every Ember workaround for a toolkit bug or gap cites a toolkit issue, and goes once the fix is
in the pinned toolkit, because a workaround without an issue is never removed. The pins are
compose-rust tag `v0.0.11` (commit `732f69a5`) and dioxus-compose
rev `94ade333` (built on compose-rust v0.0.11).

A marker has one of five forms, so that one grep finds each group at the next pin:

| Form | Meaning | At the next pin |
|---|---|---|
| `TODO(compose-rust#N)`, `TODO(dioxus-compose#N)` | The toolkit issue is open. | Read the issue state; when it is closed, check the API in the new pin and move the marker. |
| `TODO(compose-rust#N, after v0.0.11)`, `TODO(dioxus-compose#N, after 94ade333)` | The issue was closed after the pin and the API is not in the pinned source. | Confirm the API in the new pin, remove the workaround with a test, or move it to the display list. |
| `TODO(compose-rust#N, in v0.0.11)`, `TODO(dioxus-compose#N, in 94ade333)` | The API is in the pinned source. Removing the workaround changes what is drawn or how input behaves, so it waits for a builder with a display. | Nothing waits for a pin. A builder removes it and checks it on a display. |
| `v0.0.7: #N fixed; workaround kept until checked on a display` | The older wording of the form above (`#N` is a compose-rust issue). | The same as the form above. |
| `TODO(compose-rust, unfiled: <gap>)` | A real gap with no toolkit issue. | File the issue, then replace the marker with its number. |

`TODO(#673)` marks an Ember task where the toolkit already has what is needed and no toolkit
issue applies.

How the state was read: issue states come from the saved issue lists of 2026-11-15, and each
closed issue was then looked up in the pinned source. Dates alone are not used, because
dioxus-compose#256 was closed after the pinned commit while its API is in the pin.

### Counts

| Group | Marker lines |
|---|---|
| Removed in this pass | 2 |
| Closed after the pin (wait for the next pin) | 13 |
| Toolkit issue open | 19 |
| In the pin, waits for a display check (`TODO(..., in ...)`) | 27 |
| In the pin, older wording (`workaround kept until checked on a display`) | 54, plus 3 notes on compose-rust#100 |
| Unfiled toolkit gaps | 4 lines, 3 gaps |
| Ember tasks with no toolkit issue (`TODO(#673)`) | 2 |

The counts above are the marker pass's; `feat/673-drop-in-pin` and `feat/673-editor-in-pin` have
not recounted them.

The first grep of this pass found 186 lines that name a toolkit issue. The rest of them are
references to a fix that is already in use (for example `compose-rust#372` beside a container
`on_click`) and tests that guard against a removed marker coming back. They need no action.

### Removed in this pass

| Place | Workaround | Toolkit issue | State | What replaced it |
|---|---|---|---|---|
| `src/ui/cr/session.rs` `PaneTerminal` | The terminal pane opened at its top and did not follow new output. | compose-rust#385 | Closed, in the pin since v0.0.5 (`ScrollState::scroll_to`) | The pane's `ScrollColumn` takes `remember_scroll_state()` and `FollowNewest` moves it, as the transcript does. Test `pane_terminal_follows_its_newest_line_with_the_toolkit_scroll_state`. |
| `src/ui/cr/compat.rs` `SplitPane` | A fixed two-column row stood in for a split pane. It had no caller. | compose-rust#27 | Closed, in the pin (`compose_rust::ui::SplitPane`) | The function is gone. Test `no_stand_in_split_pane_is_left`. |
| `src/ui/cr/channel_owner.rs`, `add_computer.rs`, `files.rs`, `project.rs` | A sheet body took a fixed or estimated height, because a node's height could not be capped. | compose-rust#778 | Closed, in v0.0.11 (`Modifier::max_height`) | Each sheet body is a `ScrollColumn` capped with `Modifier::max_height`; `new_project_body_estimate`, `add_body_estimate` and `share_body_estimate` are gone. Tests `fr_l32_1_*` and `fr_l26_1_new_project_body_is_capped_at_the_room`. |
| `src/ui/cr/vcs.rs` (`BranchMenu`, `FilesTreeCapped`) | The branch list took a fixed 300 and the changes tree a fixed height from a row count. | compose-rust#778 | Closed, in v0.0.11 (`Modifier::max_height`) | Both take `Modifier::max_height` (`BRANCH_POP_LIST_MAX_H`, `tree_view_height(cap)`); `branch_pop_list_height` is gone. Tests `fr_l10_4_branch_menu_sizes`, `fr_l19_a_scrolling_tree_shows_whole_rows_at_rest`, `fr_l19_the_commit_box_is_in_the_changes_card_and_the_tree_leaves_it_room`. |
| `src/ui/cr/agents.rs` (`HeadedCard`) | A zero-width spacer stood in as a minimum-height strut in the card header row. | compose-rust#778, #826 | Closed in v0.0.11 (`Modifier::min_height`) | The header row takes `.min_height(CARD_HEAD_CONTENT_MIN_H)` and the strut spacer is gone. Test `fr_l20_a_card_has_a_header_row_with_an_icon_tile_and_a_rule_across_the_card`. |
| `src/ui/cr/vcs.rs` (`CircleIconButton`), `session.rs` (`IdeChevronSized`) | A press area was as large as its row at most; the IDE chevron was 26 by 32. | compose-rust#472 | Closed, in v0.0.11 (`Modifier::min_touch_target`) | Both carry `.min_touch_target(ICON_TOUCH_TARGET)` (44 dp) without growing the node. Tests `fr_l7_ide_chevron_beside_the_default_opens_the_chooser_when_clicked`, `a_fixed_name_sits_on_the_painted_box_and_a_data_name_on_a_matched_button`. |
| `src/ui/vcs.rs`, `src/ui/cr/vcs.rs` (`DiffViewAtWidth`) | The diff line check scrolled with its row. | compose-rust#424 | Closed, in v0.0.11 (`Modifier::sticky`) | The diff line check cell takes `Modifier::sticky()`. Test `fr_l19_hunk_action_sits_in_the_header_row`. |
| `src/ui/cr/components.rs` `OneLineHintField`, `new_session.rs` | Ember drew a long hint as its own one-line label under the field. | compose-rust#786 | Closed, in v0.0.11 (single-line `TextField` keeps its placeholder on one line with an ellipsis) | `OneLineHintField` and `FieldOptions::one_line_hint` are gone; the hint is passed as the field's `placeholder`. Tests `fr_l10_1_one_line_hint_is_the_toolkit_placeholder`, `fr_l10_1_new_session_long_hints_are_the_field_placeholder`. |
| `assets/editor/page.css`, `src/ui/editor_workbench.rs` | `.ed-none` stood in for closed tool windows and conditional children so placeholder comment nodes would not become grid or flex items. | dioxus-compose#266 | Closed, in `94ade333` (comment nodes make no layout box) | `.ed-none` and its four `div { class: "ed-none" }` branches are gone. Test `fr_l31_26_the_status_bar_is_32_high_and_flush_with_the_page_bottom`. |

### Removed on `feat/673-drop-in-pin`, to be confirmed by capture

Branch `feat/673-drop-in-pin` removes the workarounds below. None of it is compiled or seen on a
display yet: one builder run compiles it, runs the tests and takes the captures named in the last
column. A row moves out of this table when its capture is judged.

| Place | Workaround | Toolkit issue | What replaced it | Capture that proves it |
|---|---|---|---|---|
| `src/ui/cr/vcs.rs` (`SectionList`, the sheet rail, `PaneTabSegment`, `CircleIconButton`, 이 덩어리 전체), `agents.rs` (`SectionNav`), `routines.rs` (`GlyphButton`, the phone's 새 루틴, 편집 and 지금 실행 squares, `RunTile`), `new_channel.rs` (`NewChannelButton`) | An invisible `Button` over a painted box carried focus, role and a name from a fixed set. The three `FocusOverlay` functions, `NamedOverlay` and `DisabledOverlay` are gone. | compose-rust#429 (`Modifier::label`, `role`), #372 | The painted box is the control: `components::NamedClickable` (a clickable container with the name, the button role and the shared press values), or `NamedDisabled` for the busy 실행 중 square. Tests: `a_fixed_name_sits_on_the_painted_box_and_a_data_name_on_a_matched_button` (`vcs.rs`), `section_rows_are_the_named_controls_and_data_named_chips_keep_a_matched_button` (`agents.rs`), `a_fixed_name_sits_on_the_painted_box_and_a_routine_name_on_a_matched_button` (`routines.rs`), `fr_l12_new_channel_button_says_why_in_demo_mode` (`new_channel.rs`). | `EMBER_CR_ROUTE=vcs`, `agents`, `routines` and the channel list at 1440 and 390, light and dark: every row, pill and round button looks as before. By hand, with a pointer and a keyboard: a press scales and tints the painted box in its own shape (a 44 dp circle round the 32 dp remote buttons), Tab puts the focus ring on the box, and VoiceOver reads the same names as before. |
| `src/ui/cr/vcs.rs` (`ConflictFileRow`, `BarSelector`, `RemoteMainButton`, the tree's folder and file rows, `HistoryRow`, `PullRequestRow`, the team card's PR pill, `StashPill`), `agents.rs` (`ModelRadio`, `SelectChip`, `MenuPickerPill`), `routines.rs` (`FilterSegButton`, `RoutineRow`) | The same invisible `Button`, `fill_max_size`, for a name built from data. It widened a box that had no width. | compose-rust#431 (`Modifier::match_parent_size`); #425 does not fit (see the unfiled gaps) | One shared clear button, `components::DataNamedOverlay`, with `match_parent_size`: it takes its box's bounds and no more. The names are unchanged. Test `a_data_named_overlay_takes_its_box_and_a_disabled_named_box_has_no_click` (`components.rs`). | The same captures: no row or pill is wider or taller than before, and the team card's PR pill is as wide as its text. By hand: each of these still takes a click over its whole box and reads its name. |
| `src/ui/cr/agents.rs` (`SelectChip`, `MenuPickerPill`), `vcs.rs` (`StashPill`, 이 덩어리 전체) | A chip had a fixed 200 width and the pills a `text_px` estimate (`vcs_pill_width`, at least 64), because the `fill_max_size` button over them or a Box in a Row took the rest of the row. | compose-rust#431, #427 | No width is sent. The box takes the width of its label and its side spaces (14 dp, `VCS_PILL_PAD_X`; 12 dp and the icon in a picker pill). `vcs_pill_width` is gone. Tests `chips_and_picker_pills_send_no_width_of_their_own` (`agents.rs`), `a_small_pill_takes_the_width_of_its_label_and_14_at_each_side` (`vcs.rs`). | `EMBER_CR_ROUTE=agents` (모델 and 프로필 sections) at 1440 and 390: each chip hugs its label and the hint beside it keeps whole lines, not one word per line. `EMBER_CR_ROUTE=vcs` (스태시, 팀, 변경 사항 diff) at 1440 and 390: each pill hugs its label with 14 at each side. Two things to judge: a chip that was 200 wide is now narrower, and a short pill (세션, 적용, 삭제) is narrower than the old 64 minimum. |
| `src/ui/cr/project.rs` (`LeaderOption`) | A ring with a filled centre stood in for a radio button. | compose-rust#545 | `cu::RadioButton(m.leader)` with `ring_color(text_2)`, `dot_color(ink)`, `size(16)` and the member's name as its accessible name. It takes no `on_change`. Test `the_leader_mark_is_the_toolkit_radio_and_the_opened_search_row_asks_for_focus`. | The project settings sheet (리더 section) at 1440 and 390, light and dark: a 16 dp ring in `text-2` with an `ink` dot on the leader, no fill behind the dot. To judge: the ring's width and the dot's size are the design system's (the stand-in drew 1.5 dp and 8 dp), and the radio now takes keyboard focus though a press changes nothing (TODO(#384)). |
| `src/ui/cr/project.rs` (`Grid`, the search row at 900 dp and below) | The search field was not focused when the icon opened it. | compose-rust#410 | The field is bound to a `FocusHandle` (`SearchFieldFocus`, `FieldOptions::focus`, `TextField::focus_handle`) and an effect asks for the focus once the row is composed. Same test. | By hand at 900 dp or below: press the search icon and type at once; the text goes into the field. A capture cannot show it. |
| `src/ui/cr/project.rs` (the settings sheet's computer checks), `routines.rs` (`SegChoices`, `FilterSegButton`), `src/ui/project.rs` (`computer_check_width`) | Chips were cut into lines by an estimated width (`wrap_lines` over `computer_check_width`, `seg_lines` over `seg_width`). | compose-rust#521 | The toolkit's `FlowRow` (`components::ChoiceRow` in the routine form); a segment sends no width and pads its label 12 dp each side. `seg_lines`, `seg_width`, `seg_line_room` and `computer_check_width` are gone. Tests `fr_l25_form_segments_wrap_in_a_flow_row` (`routines.rs`), `the_settings_sheet_wraps_its_computer_checks_in_a_flow_row` (`project.rs`). | The routine form (새 루틴) at 390 and 1440 and the project settings sheet (컴퓨터) at 390 and 1440: at 390 the 프로젝트 segments take two or more lines with every option in view, 8 apart both ways, no segment squeezed or clipped; at 1440 one line. A segment shorter than 44 dp is narrower than before. |
| `src/ui/cr/home.rs` (`TokenTrendTile`), `src/ui/usage.rs` | No day of the usage chart was selectable. | compose-rust#417 | The plot's box takes `on_pointer` (press, move, leave, cancel); `usage::trend_day_at` picks the day and the line under the plot reads `usage::trend_day_line`. SPEC FR-L23.1. Tests `fr_l23_1_the_trend_plot_names_the_day_under_the_pointer`, `fr_l23_1_the_trend_plot_takes_the_pointer_and_reads_the_day_under_it`. | The home dashboard at 1440 and 390 looks as before with no pointer over the plot. By hand with a pointer: moving over a bar changes the line under the plot to that day's date and tokens, leaving restores the total, and the wheel still scrolls the page over the plot. |
| `src/ui/files.rs` (`FILE_PICKER_AVAILABLE`), `src/ui/cr/files.rs` (the tray's 파일 고르기) | `FILE_PICKER_AVAILABLE` was false and the button said the file step is not ready. | compose-rust#483 | The constant is true on macOS, the one platform with a dialog in the pin. The button opens `FileDialog::open_files()`; `files::pick_outcome` turns the answer into the send sheet's draft, nothing, or the not-ready notice. Tests `fr_l27_5_the_file_picker_follows_the_platform_and_its_answer_fills_the_draft`, `fr_l27_5_the_tray_button_opens_the_platform_file_dialog`. | The 파일 screen outside the demo on macOS at 1440: 파일 고르기 is enabled and the not-ready line under it is gone. By hand: the button opens the system panel, choosing two files opens the send sheet listing them, and Cancel changes nothing. Not verified on Windows or Linux, where the button stays off. |
| `src/ui/cr/components.rs` (`font_refs`), `theme.rs` | An unused font stack of system faces stood by "until the fonts are bundled". | compose-rust#374, Ember #216 | Nothing: the theme already carries the bundled Inter, Pretendard and JetBrains Mono stacks (`ui::fonts::app_theme`). `font_refs` and its notes are gone. Test `the_font_stack_is_the_themes_and_no_screen_helper_names_one`. | None needed: nothing drawn changes. Any existing capture confirms the faces. |
| `src/ui/cr/project.rs` (`ProjectBoxView`) | The team area was a box rounded at all four corners over the card's bottom. | compose-rust#392 | `corner_radii([0, 0, 24, 24])`: square under the rule, the card's radius at the bottom. Test `the_team_area_is_square_under_the_rule_and_round_at_the_cards_bottom`. | The project list at 1440 and 390, light and dark: the `surface-2` team area meets the rule with square corners and follows the card's bottom corners with no `surface` sliver showing inside the border. |
| `src/ui/cr/project.rs` (`ListActions`, the search button at 900 dp and below) | The button took a toolkit glyph, with no name and no open state. | compose-rust#428, #429 | Ember's search SVG in the 40 px ring, named 팀원 검색 열기 (`a11y::OPEN_MEMBER_SEARCH`) with `Modifier::expanded`. Test `fr_l26_the_header_search_button_is_named_and_says_whether_its_row_is_open`. | The project list at 900 and 390: a 40 px ring with the 1.5 px line search glyph at 18 px in `text-2`, the same size and place as the other header buttons. By hand: VoiceOver reads 팀원 검색 열기 and whether it is expanded. |

### Removed on `feat/673-editor-in-pin`, to be confirmed by capture

Branch `feat/673-editor-in-pin` removes the editor window's workarounds below. None of it is
compiled or seen on a display yet, for the same reason as the table above.

| Place | Workaround | Toolkit issue | What replaced it | Capture or hand check that proves it |
|---|---|---|---|---|
| `src/ui/editor_scm.rs` (`diff_editor_view::diff_editor`), `src/ui/editor.rs` (`diff_line_tints`, `with_diff_line_tints`) | A change in the editable diff showed as gutter bars only. | compose-rust#542 | Each run of added or changed lines gets a `Decoration::line_background` in `DiffAdded` or `DiffModified`, after the document's own decorations; a removed line keeps its gutter dot. Tests `fr_l31_10_added_and_changed_lines_are_tinted_in_runs` (`editor.rs`), `fr_l31_10_the_editable_diff_sends_the_line_tints` (`editor_scm.rs`). | The editor window with a changed file's working diff open, light and dark: added lines tinted green, changed lines in the modified role, the full width and past each line's end, the gutter bars as before. By hand with the keyboard: type on a tinted line and the tint follows it. |
| `src/ui/editor_scm.rs` (`base_rows`, `diff_view_zones`, `editable_body`, `diff_editor_view::diff_editor`) | Removed base lines were a red rule over the next row and a hover list, because the editor had no blank rows opposite them. | compose-rust#543 | The editor gets a `view_zones` zone after the line above each removed run, as many rows high as lines were removed, and the base side draws each removed line as a red row of its own (`BaseRow::Removed`). Lines removed above the first line keep the rule: no zone can precede line 0 (unfiled gap below). Test `fr_l31_10_removed_lines_take_rows_opposite_view_zones`, `fr_l31_10_the_editable_diff_sends_zones_and_follows_the_pixel_scroll`. | The editor window with a working diff that removes lines in the middle and at the end, light and dark: each removed line is a red row on the left with a blank row of the same height on the right, and the rows below stay level on both sides. By hand with the keyboard: the caret passes over the blank rows. |
| `src/ui/editor_scm.rs` (`DiffScroll`, `ScmState::diff_scroll`, `set_diff_scroll`, `set_diff_row_px`) | The base side followed the editor's first visible line (`on_visible_range_change`, `set_diff_lines`), so it jumped by whole lines. | compose-rust#544 | `on_code_scrolled` and `on_code_metrics` give the offset and the row height in physical pixels; their ratio is the rows above the top with a fraction, and the base side draws from that row shifted up by the fraction of its own `--code-lh`. `diff_lines` and `set_diff_lines` are gone. `set_scroll_offset` is not used: the base side has no scroll of its own. Tests `fr_l31_10_the_base_side_follows_the_editor_by_pixels`, `fr_l31_10_reading_the_same_diff_again_keeps_it_editable` (updated), and the wiring test above. | By hand with a trackpad or wheel over the editor in a long working diff, at scale 1 and 2: the base rows move smoothly with the editor's and stay level with them, mid-line too. |
| `src/ui/editor_language.rs` (`popup_style`, `view::set_caret_rect`, `view::overlay`), `src/ui/editor_workbench.rs` (`editor_pane`) | A list was a sheet over the window, not a popup at the caret. | compose-rust#553 | The editor's `on_caret_rect` is kept and a list opens 2 px under the caret, inside the window; before the first report it is the sheet. Left: the parameter hint stays at the bottom of the editor area (`TODO(#673)`, it sits in `.ed-main`, whose place the page does not know, and #800 moved it off a window-fixed box). Tests `fr_l31_13_a_list_opens_under_the_caret`, `fr_l31_13_the_code_editor_takes_keys_pointer_and_caret`. | `EMBER_EDITOR_LANG_SHEET=completion` and `actions` with the caret mid-file and near the right and bottom edges, light and dark: the list sits under the caret line and stays inside the window. By hand with the keyboard: Ctrl+Space after moving the caret opens the list at the new place. |
| `src/ui/editor_language.rs` (`code_key_action`, `view::code_key`), `src/ui/editor_workbench.rs` (`editor_pane`, the 코드 menu comment) | The language chords reached the page only when the focused code did not keep the key; the menu note said the menu was there for that. | compose-rust#554 | `on_key_preview` runs `code_key_action` (F2 and F12 by name, the rest by the DOM key string) and consumes a chord; Escape there closes the parameter hint. The menu keeps the commands, as Code-OSS's does. Left: a list still takes the focus and is filtered in its own field (`TODO(#673)`). Test `fr_l31_13_the_focused_code_offers_the_chords` and the wiring test. | By hand with the keyboard, the caret in the code: F12, Shift+F12, F2, Ctrl+Space, Ctrl+Shift+Space and Cmd/Ctrl+. each open their feature once (not twice) and insert nothing; ordinary typing is unchanged. |
| `src/ui/editor_language.rs` (`pointer_definition`, `view::code_pointer`), `src/ui/editor_workbench.rs` (`editor_pane`) | No Cmd or Ctrl click to a definition. | compose-rust#555 | `pointer_kinds: DOWN` and `on_code_pointer`: a primary press with Cmd on macOS or Ctrl elsewhere, over a character, asks for `Definition` there and is consumed, so the caret stays. Test `fr_l31_13_cmd_or_ctrl_click_goes_to_the_definition`. | By hand with a pointer on macOS: Cmd+click on a name in a file with a language extension jumps to its definition; a plain click and a drag select as before; Ctrl+click opens no definition. |
| `src/ui/editor.rs` (`DoubleClick`, `DOUBLE_CLICK_MS`), `src/ui/editor_workbench.rs` (`DOUBLE_CLICK`, the tree row's and the tab's `onclick`) | Two clicks on one tab or tree row within 500 ms stood in for a double-click. | compose-rust#417; dioxus-compose `f0e5b9f5` sends `dblclick` on an `Up` with `click_count == 2` (`dom/pointer.rs`) | The rows' and tabs' own `ondoubleclick` keeps the preview; `onclick` only opens or activates. The struct, the constant, the thread-local and test `fr_l31_tabs_two_clicks_on_one_target_within_500_ms_are_a_double_click` (`tests/editor.rs`) are gone. Test `fr_l31_5_double_click_and_right_click_are_the_real_events` (`editor_workbench_glue_tests.rs`). | By hand with a pointer in the editor window: a single click on a file opens an italic preview tab, a double-click on the row or the tab makes it permanent, two slow clicks do not; a double-click on a terminal tab starts its rename. |
| `src/ui/editor_tree_menu.rs` (`PLUS_MENU_HOLDS_SELECTION`, `SELECTION_HEADING`, `TreeMenuEntry::Heading`), `src/ui/editor_workbench.rs` (the heading arm) | The header "+" menu carried a "선택한 항목" section with the context menu's items. | compose-rust#417; dioxus-compose `f0e5b9f5` sends `contextmenu` | The "+" menu is 새 파일 and 새 폴더 only; the row's `oncontextmenu` opens the context menu at the pointer. Shift+F10 and the context-menu key stay: they are Code-OSS's keyboard route, not a stand-in. Test `design_rule1_plus_menu_is_new_file_and_folder_only` (replaces `design_rule1_plus_menu_is_new_file_and_folder_then_the_stand_in_section`). Left: the ⋯ menu's current-tab group, where Shift+F10 on the tab strip leads (`TODO(#673)`: open the tab's own menu at the tab from the keyboard). | The project window's "+" menu at 1440, light and dark: two items. By hand with a pointer: right-click a tree row and a tab, each menu opens at the pointer with its items; right-click on the scrim closes it. By hand with the keyboard: Shift+F10 on a tree row opens the context menu. |

### Closed after the pin

| Place | Workaround | Toolkit issue | State | Action at the next pin |
|---|---|---|---|---|
| `src/ui/cr/components.rs` (`NameThenPill`) | A name and a pill in a row are measured through `observe_size_every_dp`, because no weight leaves a child its own size. | compose-rust#778 | Closed 2026-11-14T16:50Z with `Modifier::max_height` / `size_in`; `Modifier::weight` in v0.0.11 still fills its share (`fill = false` is not exposed). | Use a weight without fill when exposed and delete the measuring. |

### Toolkit issue open

| Place | Workaround | Toolkit issue | State | Action when it is fixed |
|---|---|---|---|---|
| `android/MainActivity.kt:6`, `src/android.rs:59`, `src/ui/back.rs:7` | The Android Activity calls Ember's `back` itself. | compose-rust#302 | Open | Take the toolkit's BackRequested event and delete the JNI entry. |
| `src/ui/vcs.rs:18`, `src/ui/cr/vcs.rs:2586` | The history is a list with a dot, with no lane graph. | compose-rust#308 | Open | Draw the graph on a `Canvas` with Path commands. |
| `src/ui/cr/shell.rs:412` | The bottom bar has no backdrop blur. | compose-rust#300 | Open | Add the blur and the safe-area inset. |
| `src/ui/cr/vcs.rs:344` `StageCheck` | A painted square lies over an invisible toolkit checkbox; a partial check reports unchecked. | compose-rust#767 | Open | Use the toolkit `Checkbox` with its shape and partial state. |
| `src/ui/cr/components.rs:950` | The frame and the 12 px inset sit on a box around a bare field. | compose-rust#771 | Open | Move them onto the field. |
| `src/ui/cr/components.rs` (`PopoverMenu`), `home.rs` (`mode_menu`) | A popover menu's arrow keys and focus on the selected row work on macOS (checked 2026-11-21, #888), but the row the keys move to shows no mark, and a bottom sheet at 560 and below has no menu keyboard. | compose-rust#810 | Open (keys in the pinned compose-rust); the mark is compose-rust#941, sheets #940 | Check the mark again when the renderer draws one; use the menu keyboard in sheets when the toolkit offers it. |
| `src/ui/cr/home.rs` (`model_body`, `ModelRow`) | The Home model picker's search is a plain text field and its rows are list items in a list named 모델, selected on the drawn active row, where the mockup has a combobox, a listbox of options and `aria-activedescendant` ([leader, 2026-11-21], #888). | compose-rust#942 | Open | Make the search a combobox tied to the active option and the rows options when the roles and the relation land. |
| `src/ui/cr/home.rs:918` | The model popover's row takes a fixed height, not `equal_height`. | compose-rust#766 | Open | Use `equal_height` again. |
| `src/ui/cr/project.rs:473` | The add chip has a solid border where the mockup has a dashed one. | compose-rust#817 | Open | Set the stroke dash. |
| `src/ui/cr/project.rs:457` | The strip's scroll step is taken from the window's width. | compose-rust#818 | Open | Read the viewport size from `ScrollState`. |
| `src/ui/cr/components.rs` (`EmberCardSized`, `InsetPanelSized`), `src/ui/cr/channel_owner.rs` | `Modifier::padding` has no per-side builder (`PaddingEach` is on the wire only) and `TextField` has no read-only flag; `Modifier::min_height` landed in v0.0.11. | compose-rust#826 | Open | Use per-side padding and `read_only(true)` when they land. |
| `src/ui/cr/components.rs` (`DataNamedOverlay`) | A clickable container takes its name from `Modifier::label` (`LabelCache`, 128-entry LRU in v0.0.11) and has no per-node owned name. | compose-rust#831 | Open | Move data-named boxes onto `Clickable` once a container takes an owned name. |
| `src/lib.rs:206`, `src/ide.rs:109`, `ide.rs:167`, `src/ui/coexist.rs:281` | The editor is its own process started with `--editor`. | dioxus-compose#161 | Open. dioxus-compose#152 (the `Window` element) is closed and in the pin. Since dioxus-compose `a64f3b92` (#161: events reach a `WidgetView` inside an `HtmlView` under a Compose root) the editor window is a Compose root holding the page in an `HtmlView` (#301, `editor_workbench::view::EditorRoot`), but it is still its own process. | Open the editor as a `Window` node on the shell's host and delete `editor_command`. |
| `src/ui/editor_workbench.rs:3086` | Paste and IME composition handlers run in tests but not on screen. | dioxus-compose#205 | Open | Check the terminal's paste and composition on a display, then drop the note. |

### In the pin, waits for a display check

A builder takes these one screen at a time. Each removal changes what is drawn or how input
behaves, so a capture or a run decides it.

| Place | Workaround | Toolkit issue | State | Action |
|---|---|---|---|---|
| `src/ui/cr/routines.rs` (`ListPane`, the filter row) | The project filter chips scroll sideways in a `ScrollRow`. Their width estimate (`seg_width`) is gone, see the removed rows. | compose-rust#427, #328 | In the pin since v0.0.7 | A capture of `EMBER_CR_ROUTE=routines` at 1440 with six or more projects must show whether the sixth chip still clips; if it does not, the row can become a `ChoiceRow`. |
| `src/ui/cr/vcs.rs` (`ChangesPane`, `PaneLayout::Two`), `home.rs:328` (`ROW_TILE_H`) | Panes scroll on their own; tiles take a fixed height. | compose-rust#389 | In the pin (`Row::equal_height`, confirmed in `ui/widgets.rs`). Kept on `feat/673-drop-in-pin`: an `equal_height` row is sized by intrinsic heights, which is wrong for wrapped text and counts a scrolling child as 0 (compose-rust#766, open), and the diff pane holds wrapped code. | A capture of the two panes in one page scroll with `equal_height` must show the files card as tall as the diff and still in view while the diff scrolls. For the home tiles: the three tiles of the success row at 1440 and 1024 must be equally tall with `EmberCardSized` and no `ROW_TILE_H`. |
| `src/ui/home.rs:1313` (`CHIP_WRAP_WIDTH`) | The Home composer puts its mode and computer chips and the send button on a second line below 1100 dp, by window width. | compose-rust#521 | In the pin (`FlowRow`, already used by `components::ChoiceRow`). Kept on `feat/673-drop-in-pin`: the split also moves the send button, which a `FlowRow` would wrap with the chips instead of keeping at the row's end. | A capture of the composer at 1440, 1100, 1024 and 390 with the chips in a `FlowRow` must show the send button at the end of the last line at every width. |
| `src/terminal_input.rs:145`, `src/ui/editor_workbench.rs` (`terminal_body`, the paste chord) | A paste chord in the editor terminal shows a notice. | compose-rust#546 | In the pin (`compose_rust::clipboard::read_text(callback)`); dioxus-compose's HTML runtime makes the same call for its own Cmd+V, so the read answers in the editor window. Left on `feat/673-editor-in-pin` too: a multi-line paste must first ask (`needs_multiline_warning`, Code-OSS's dialog) and the window has no confirmation for it. The editor side's marker is `TODO(#673)`; `terminal_input.rs` is outside that branch. | Build the multi-line confirmation, read the clipboard on the chord, send `paste_bytes`, and check it in the editor window. |
| `src/ui/editor_language.rs` module notes | A snippet is inserted as plain text. | compose-rust#556 | In the pin (`CodeEditorHandle::insert_snippet`). The session hands the window an `ember_editor::WidgetCommand::EditCode` with the expanded text, so `ember/editor` has to carry the snippet first; that is outside this branch, so the marker is `TODO(#673)`. | Carry the snippet in `WidgetCommand` and open a snippet session with `insert_snippet`. |
| `src/ui/editor_status.rs:110` `list_window` | A picker list is windowed around the highlighted row. | dioxus-compose#200 | In the pin (`MountedData::scroll_to`). Kept on `feat/673-editor-in-pin`: with the windowed list removed, a capture of the language picker on a JSON file at 1440 showed the list at its top and the highlighted JSON row out of view, also with a reveal sent on every render, so the request does not move this list. Rechecked by reading on 2026-11-22 (#838): the into-view path is unchanged on dioxus-compose `5b6f707c` and compose-rust `e52fbd39`, and no toolkit test covers a request asked from `onmounted` in the creating batch for a row inside an HTML island's both-axes `overflow:auto` list. The first try also asked only from `onmounted`, before the highlight moved to the current entry. | Scroll the row into view and delete `list_window` once a capture shows the row in view. |
| `src/ui/cr/components.rs:1882` | The first composition falls back when the window reads 0x0. The editor side (a fixed height allowance) is already gone: the page follows the viewport (`editor::editor_page_size_style`, #301), checked on `feat/673-editor-in-pin`. | compose-rust#467, #479 | In the pin (`with_report_size`) | Follow the reported size. |
| `src/ui/cr/channel_owner.rs:559`, `channels.rs:357`, `home.rs:1508`, `launcher.rs:171`, `new_channel.rs:242`, `new_session.rs:5`, `project.rs:1200`, `project.rs:1712` | Text fields are uncontrolled. | compose-rust#374 | In the pin (`TextField::text`) | Kept: each field needs its value held in state first. By hand, in each field: type in the middle of a Korean word with the IME composing; the caret stays where it is, no syllable is dropped or doubled, and a programmatic value (a suggestion, a reset) replaces the text. A capture cannot show this. |
| `src/ui/cr/components.rs:1998`, `channels.rs:627`, `project.rs:371` | A field's frame is drawn on a box around a bare field. | compose-rust#376, #375 | In the pin | Kept: compose-rust#771 (open) still keeps the 12 px inset on a box. A capture of the search fields at 1440 and 390, light and dark, with the frame on the field must show the same 1 px `line` ring, the pill ends and the placeholder centred in the height. |
| `src/ui/cr/components.rs:199`, `components.rs:238` | A modal list stands in for a dropdown popup. | compose-rust#383 | In the pin (`Select`, `Dropdown::closed`) | Kept. A capture of a toolkit `Select` open under its field must show an opaque `surface` box with a 1 px `line` border and the `corner-md` radius, 38 px rows with a left-aligned `text` label, the chosen row on `sel`, opening below the anchor. |
| `src/ui/cr/theme.rs:341`, `components.rs:1849` | Corners are plain arcs. | compose-rust#294 | In the pin (`Modifier::corner_curve`) | Kept: the value goes on every card, sheet and rounded square at once. Captures of the home, project and session screens and a sheet at 1440, in both schemes, with `corner_curve(0.6)` must match the mockup's corners where it sets `corner-shape: squircle`, with the border and shadow following the same curve and no gap at a clipped child. |
| `src/ui/cr/session.rs:5170`, `components.rs:1806` | Escape does not close the drawer; a modal passes its close callback by hand. | compose-rust#476 | In the pin (`on_key_down`, `request_focus`) | Kept. By hand with a keyboard: open the drawer, press Escape, and the drawer closes and the focus returns to the button that opened it; the same for a modal sheet. A capture cannot show it. |
| `src/ui/cr/session.rs:1285` | The terminal rows are not in a `SelectionContainer`. | compose-rust#5 | In the pin | Kept. By hand with a pointer: drag across several terminal rows; the selection follows the text of each styled row, copies in reading order with line breaks, and a tap on a row's control still works. |
| `src/ui/cr/home.rs:438`, `session.rs:3794`, `project.rs:1347`, `files.rs:506`, `characters_screen.rs:200`, `conversation.rs:257` | `EMBER_CR_SCROLL` leaves blocks out for review captures. | compose-rust#385 | In the pin (`ScrollState::scroll_to`) | Kept: the review capture script reads `EMBER_CR_SCROLL`. It goes when the script scrolls for real; the captures of the lower half of each of the six screens must then match the ones taken with blocks left out. |
| `src/ui/cr/home.rs:2087` | The stripes take a fixed height. | compose-rust#388 | In the pin (`Canvas::view_box`) | Kept. A capture of the home 승인 대기 tile at 1440, 1024 and 390 with the stripes in a `view_box` canvas that fills its block must show the stripes reaching both edges of every block, at the same angle and spacing in each. |
| `src/ui/vcs.rs:16` | A text Button is the hit target of a row or a diff line. | compose-rust#372 | In the pin (container `on_click`) | Migrate the VCS rows. |

### Ember tasks with no toolkit issue

| Place | Note | Action |
|---|---|---|
| `src/ui/term_pane.rs:15` | 500 lines are drawn at once. The toolkit has `LazyColumn` (`compose-rust/src/ui/lazy.rs`). | Draw the lines in a `LazyColumn` and check it on a display. |
| `src/ui/cr/compat.rs:146` `CodeText` | Long lines wrap. The toolkit has `ScrollRow`. | Put the text in a `ScrollRow` and check it on a display. |

### Unfiled toolkit gaps

These have no compose-rust issue in the saved list. The leader files them; the marker then takes
the number. (`compose-rust#826` was filed for per-side padding, minimum height and read-only
`TextField`, and `compose-rust#831` for an owned container accessible name; `Modifier::min_height`
landed in v0.0.11.)

| Place | Gap | Toolkit source |
|---|---|---|
| `src/ui/editor_scm.rs` (`editable_body`) | A code editor's view zone follows a line (`ViewZone::after_line`), so none can stand above line 0. Base lines removed above a diff's first line stay a red rule and a hover list on the first row. | `compose-rust/src/code.rs` (`ViewZone`), `renderer/desktop/src/renderer/CodeEditorSurface.kt:591` |

### Notes not changed in this pass

`src/ui/project.rs:12`, `src/ui/session.rs:14` and `src/ui/characters.rs:9` list gaps of the
earlier Dioxus screens without numbers ("no clickable Row", "no back stack", "no animation").
They describe model files, not the compose-rust screens. The matching toolkit issues are
compose-rust#372 (in the pin), #302 (open) and the animation API (in the pin). They are left as
they are until those files are next edited.

## 한국어

### 규칙

툴킷의 버그나 빈 곳을 피해 가는 Ember 코드는 모두 툴킷 이슈 번호를 적고, 고친 툴킷을 핀으로
잡으면 지운다. 이슈가 없는 우회 코드는 지울 때를 알 수 없기 때문이다. 핀은 compose-rust 태그
`v0.0.11`(커밋 `732f69a5`)과 dioxus-compose 리비전 `94ade333`
(compose-rust v0.0.11 기준)이다.

표시는 다섯 가지 꼴 중 하나다. 다음 핀 때 grep 한 번으로 묶음을 찾기 위해서다.

| 꼴 | 뜻 | 다음 핀에서 할 일 |
|---|---|---|
| `TODO(compose-rust#N)`, `TODO(dioxus-compose#N)` | 툴킷 이슈가 열려 있다. | 이슈 상태를 읽는다. 닫혔으면 새 핀에서 API를 확인하고 표시를 옮긴다. |
| `TODO(compose-rust#N, after v0.0.11)`, `TODO(dioxus-compose#N, after 94ade333)` | 이슈가 핀 뒤에 닫혔고 핀 소스에 API가 없다. | 새 핀에서 API를 확인하고 테스트와 함께 우회 코드를 지우거나, 화면 확인 목록으로 옮긴다. |
| `TODO(compose-rust#N, in v0.0.11)`, `TODO(dioxus-compose#N, in 94ade333)` | API가 핀 소스에 있다. 지우면 그림이나 입력 동작이 바뀌므로 화면을 볼 수 있는 빌더를 기다린다. | 핀을 기다리지 않는다. 빌더가 지우고 화면에서 확인한다. |
| `v0.0.7: #N fixed; workaround kept until checked on a display` | 바로 위 꼴의 예전 표현이다(`#N`은 compose-rust 이슈). | 바로 위와 같다. |
| `TODO(compose-rust, unfiled: <빈 곳>)` | 실제로 빈 곳인데 툴킷 이슈가 없다. | 이슈를 올리고 표시에 번호를 적는다. |

`TODO(#673)`은 툴킷에 필요한 것이 이미 있어 툴킷 이슈가 필요 없는 Ember 쪽 일을 가리킨다.

상태를 읽은 방법: 이슈 상태는 2026-11-15에 저장한 이슈 목록에서 읽었고, 닫힌 이슈는 모두 핀
소스에서 다시 찾았다. 날짜만으로 판단하지 않는다. dioxus-compose#256은 핀 커밋 뒤에 닫혔지만
API는 핀에 있다.

### 개수

| 묶음 | 표시 줄 수 |
|---|---|
| 이번에 지움 | 2 |
| 핀 뒤에 닫힘(다음 핀을 기다림) | 13 |
| 툴킷 이슈 열림 | 19 |
| 핀에 있음, 화면 확인을 기다림(`TODO(..., in ...)`) | 27 |
| 핀에 있음, 예전 표현(`workaround kept until checked on a display`) | 54, 그리고 compose-rust#100 메모 3 |
| 이슈 없는 툴킷 빈 곳 | 4줄, 3건 |
| 툴킷 이슈 없는 Ember 일(`TODO(#673)`) | 2 |

위 개수는 표시 작업 때의 값이다. `feat/673-drop-in-pin`과 `feat/673-editor-in-pin`에서는 다시 세지
않았다.

이번 작업의 첫 grep은 툴킷 이슈를 적은 줄 186개를 찾았다. 나머지는 이미 쓰고 있는 수정을
가리키는 참조(예: 컨테이너 `on_click` 옆의 `compose-rust#372`)와, 지운 표시가 돌아오지 않게
막는 테스트다. 할 일이 없다.

### 이번에 지움

| 위치 | 우회 코드 | 툴킷 이슈 | 상태 | 대신 쓰는 것 |
|---|---|---|---|---|
| `src/ui/cr/session.rs` `PaneTerminal` | 터미널 칸이 맨 위에서 열리고 새 출력을 따라가지 않았다. | compose-rust#385 | 닫힘, v0.0.5부터 핀에 있음(`ScrollState::scroll_to`) | 칸의 `ScrollColumn`이 `remember_scroll_state()`를 받고, 대화와 같이 `FollowNewest`가 옮긴다. 테스트 `pane_terminal_follows_its_newest_line_with_the_toolkit_scroll_state`. |
| `src/ui/cr/compat.rs` `SplitPane` | 고정 두 칸 행이 분할 창을 대신했다. 부르는 곳이 없었다. | compose-rust#27 | 닫힘, 핀에 있음(`compose_rust::ui::SplitPane`) | 함수를 지웠다. 테스트 `no_stand_in_split_pane_is_left`. |
| `src/ui/cr/channel_owner.rs`, `add_computer.rs`, `files.rs`, `project.rs` | 노드 높이에 상한을 둘 수 없어 시트 본문 높이를 고정하거나 어림했다. | compose-rust#778 | 닫힘, v0.0.11에 있음(`Modifier::max_height`) | 시트 본문이 `Modifier::max_height`로 상한을 둔 `ScrollColumn`을 쓰고 `new_project_body_estimate`, `add_body_estimate`, `share_body_estimate`를 지웠다. 테스트 `fr_l32_1_*`, `fr_l26_1_new_project_body_is_capped_at_the_room`. |
| `src/ui/cr/vcs.rs`(`BranchMenu`, `FilesTreeCapped`) | 브랜치 목록은 300으로, 변경 트리는 행 수에서 구한 높이로 고정했다. | compose-rust#778 | 닫힘, v0.0.11에 있음(`Modifier::max_height`) | 둘 다 `Modifier::max_height`(`BRANCH_POP_LIST_MAX_H`, `tree_view_height(cap)`)를 쓰고 `branch_pop_list_height`를 지웠다. 테스트 `fr_l10_4_branch_menu_sizes`, `fr_l19_a_scrolling_tree_shows_whole_rows_at_rest`, `fr_l19_the_commit_box_is_in_the_changes_card_and_the_tree_leaves_it_room`. |
| `src/ui/cr/agents.rs`(`HeadedCard`) | 폭 0 스페이서가 카드 헤더 행의 최소 높이 버팀목을 대신했다. | compose-rust#778, #826 | v0.0.11에 있음(`Modifier::min_height`) | 헤더 행이 `.min_height(CARD_HEAD_CONTENT_MIN_H)`를 받고 버팀목 스페이서를 지웠다. 테스트 `fr_l20_a_card_has_a_header_row_with_an_icon_tile_and_a_rule_across_the_card`. |
| `src/ui/cr/vcs.rs`(`CircleIconButton`), `session.rs`(`IdeChevronSized`) | 누르는 영역이 행 높이를 넘지 못했고 IDE 화살표는 26x32였다. | compose-rust#472 | 닫힘, v0.0.11에 있음(`Modifier::min_touch_target`) | 둘 다 노드를 키우지 않는 `.min_touch_target(ICON_TOUCH_TARGET)`(44 dp)를 쓴다. 테스트 `fr_l7_ide_chevron_beside_the_default_opens_the_chooser_when_clicked`, `a_fixed_name_sits_on_the_painted_box_and_a_data_name_on_a_matched_button`. |
| `src/ui/vcs.rs`, `src/ui/cr/vcs.rs`(`DiffViewAtWidth`) | diff 줄의 체크가 행과 함께 가로로 스크롤됐다. | compose-rust#424 | 닫힘, v0.0.11에 있음(`Modifier::sticky`) | diff 줄 체크 칸이 `Modifier::sticky()`를 쓴다. 테스트 `fr_l19_hunk_action_sits_in_the_header_row`. |
| `src/ui/cr/components.rs` `OneLineHintField`, `new_session.rs` | 긴 힌트를 Ember가 필드 아래에 한 줄 라벨로 그렸다. | compose-rust#786 | 닫힘, v0.0.11에 있음(한 줄 `TextField`가 플레이스홀더를 한 줄 말줄임표로 유지함) | `OneLineHintField`와 `FieldOptions::one_line_hint`를 지우고 힌트를 필드의 `placeholder`로 넘긴다. 테스트 `fr_l10_1_one_line_hint_is_the_toolkit_placeholder`, `fr_l10_1_new_session_long_hints_are_the_field_placeholder`. |
| `assets/editor/page.css`, `src/ui/editor_workbench.rs` | 자리 표시 주석 노드가 그리드나 플렉스 항목이 되지 않게 `.ed-none`이 닫힌 도구 창과 조건부 자식을 대신했다. | dioxus-compose#266 | 닫힘, `94ade333`에 있음(주석 노드가 레이아웃 상자를 만들지 않음) | `.ed-none`과 네 곳의 `div { class: "ed-none" }` 갈래를 지웠다. 테스트 `fr_l31_26_the_status_bar_is_32_high_and_flush_with_the_page_bottom`. |

### `feat/673-drop-in-pin`에서 지움, 캡처로 확인할 것

`feat/673-drop-in-pin` 브랜치가 아래 우회 코드를 지운다. 아직 컴파일하지 않았고 화면에서 보지도
않았다. 빌더가 한 번에 컴파일하고 테스트를 돌린 뒤 마지막 칸의 캡처를 찍는다. 캡처를 판단한 행은
이 표에서 뺀다.

| 위치 | 우회 코드 | 툴킷 이슈 | 대신 쓰는 것 | 확인할 캡처 |
|---|---|---|---|---|
| `src/ui/cr/vcs.rs`(`SectionList`, 시트 레일, `PaneTabSegment`, `CircleIconButton`, 이 덩어리 전체), `agents.rs`(`SectionNav`), `routines.rs`(`GlyphButton`, 폰의 새 루틴과 편집, 지금 실행 네모, `RunTile`), `new_channel.rs`(`NewChannelButton`) | 그린 상자 위의 보이지 않는 `Button`이 포커스, 역할, 정해진 이름을 맡았다. `FocusOverlay` 함수 셋과 `NamedOverlay`, `DisabledOverlay`를 지웠다. | compose-rust#429(`Modifier::label`, `role`), #372 | 그린 상자가 곧 컨트롤이다. `components::NamedClickable`(이름, 버튼 역할, 공통 누름 값을 가진 클릭 컨테이너)을 쓰고, 바쁜 실행 중 네모는 `NamedDisabled`를 쓴다. 테스트: `a_fixed_name_sits_on_the_painted_box_and_a_data_name_on_a_matched_button`(`vcs.rs`), `section_rows_are_the_named_controls_and_data_named_chips_keep_a_matched_button`(`agents.rs`), `a_fixed_name_sits_on_the_painted_box_and_a_routine_name_on_a_matched_button`(`routines.rs`), `fr_l12_new_channel_button_says_why_in_demo_mode`(`new_channel.rs`). | `EMBER_CR_ROUTE=vcs`, `agents`, `routines`와 채널 목록을 1440과 390, 라이트와 다크로 찍는다. 행, 알약, 둥근 버튼이 모두 전과 같아야 한다. 포인터와 키보드로 직접: 누르면 그린 상자가 제 모양대로 줄고 물든다(32 dp 원격 버튼은 44 dp 원). Tab을 누르면 포커스 링이 상자에 생긴다. VoiceOver가 전과 같은 이름을 읽는다. |
| `src/ui/cr/vcs.rs`(`ConflictFileRow`, `BarSelector`, `RemoteMainButton`, 트리의 폴더와 파일 행, `HistoryRow`, `PullRequestRow`, 팀 카드의 PR 알약, `StashPill`), `agents.rs`(`ModelRadio`, `SelectChip`, `MenuPickerPill`), `routines.rs`(`FilterSegButton`, `RoutineRow`) | 데이터로 만든 이름을 위한 같은 `Button`이 `fill_max_size`였다. 폭이 없는 상자를 넓혔다. | compose-rust#431(`Modifier::match_parent_size`). #425는 맞지 않는다(이슈 없는 빈 곳 참고) | 공용 투명 버튼 하나, `components::DataNamedOverlay`가 `match_parent_size`를 쓴다. 상자 크기만큼만 차지한다. 이름은 그대로다. 테스트 `a_data_named_overlay_takes_its_box_and_a_disabled_named_box_has_no_click`(`components.rs`). | 같은 캡처에서 어떤 행이나 알약도 전보다 넓거나 높지 않고, 팀 카드의 PR 알약은 글자 폭만큼이다. 직접: 상자 전체가 클릭을 받고 이름을 읽는다. |
| `src/ui/cr/agents.rs`(`SelectChip`, `MenuPickerPill`), `vcs.rs`(`StashPill`, 이 덩어리 전체) | 위에 얹은 `fill_max_size` 버튼이나 Row 안의 Box가 남은 폭을 다 가져가서, 칩은 폭 200 고정이고 알약은 `text_px` 어림값(`vcs_pill_width`, 최소 64)을 썼다. | compose-rust#431, #427 | 폭을 보내지 않는다. 상자는 라벨과 양옆 여백(14 dp, `VCS_PILL_PAD_X`. 고르기 알약은 12 dp와 아이콘)만큼의 폭을 갖는다. `vcs_pill_width`를 지웠다. 테스트 `chips_and_picker_pills_send_no_width_of_their_own`(`agents.rs`), `a_small_pill_takes_the_width_of_its_label_and_14_at_each_side`(`vcs.rs`). | `EMBER_CR_ROUTE=agents`(모델, 프로필 구역)를 1440과 390으로: 칩이 라벨에 맞고, 옆의 설명이 한 줄에 한 단어가 아니라 온전한 줄로 보인다. `EMBER_CR_ROUTE=vcs`(스태시, 팀, 변경 사항 diff)를 1440과 390으로: 알약이 라벨과 양옆 14에 맞는다. 판단할 것 둘: 폭 200이던 칩이 좁아졌고, 짧은 알약(세션, 적용, 삭제)이 예전 최소 64보다 좁다. |
| `src/ui/cr/project.rs`(`LeaderOption`) | 가운데를 채운 고리가 라디오 버튼을 대신했다. | compose-rust#545 | `cu::RadioButton(m.leader)`에 `ring_color(text_2)`, `dot_color(ink)`, `size(16)`을 주고 접근성 이름은 팀원 이름이다. `on_change`는 주지 않는다. 테스트 `the_leader_mark_is_the_toolkit_radio_and_the_opened_search_row_asks_for_focus`. | 프로젝트 설정 시트(리더 구역)를 1440과 390, 라이트와 다크로: `text-2` 색 16 dp 고리이고 리더에는 `ink` 점이 있으며 점 뒤에 채움이 없다. 판단할 것: 고리 두께와 점 크기는 디자인 시스템 값이다(대역은 1.5 dp와 8 dp를 그렸다). 라디오가 이제 키보드 포커스를 받지만 눌러도 바뀌지 않는다(TODO(#384)). |
| `src/ui/cr/project.rs`(`Grid`, 900 dp 이하의 검색 줄) | 아이콘으로 검색 필드를 열어도 포커스를 받지 않았다. | compose-rust#410 | 필드를 `FocusHandle`에 묶고(`SearchFieldFocus`, `FieldOptions::focus`, `TextField::focus_handle`), 줄이 구성되면 이펙트가 포커스를 요청한다. 같은 테스트. | 900 dp 이하에서 직접: 검색 아이콘을 누르고 바로 입력하면 글자가 필드에 들어간다. 캡처로는 볼 수 없다. |
| `src/ui/cr/project.rs`(설정 시트의 컴퓨터 체크), `routines.rs`(`SegChoices`, `FilterSegButton`), `src/ui/project.rs`(`computer_check_width`) | 칩을 어림한 폭으로 줄에 나눴다(`computer_check_width`와 `wrap_lines`, `seg_width`와 `seg_lines`). | compose-rust#521 | 툴킷의 `FlowRow`를 쓴다(루틴 폼은 `components::ChoiceRow`). 선택지는 폭을 보내지 않고 라벨 양옆에 12 dp를 둔다. `seg_lines`, `seg_width`, `seg_line_room`, `computer_check_width`를 지웠다. 테스트 `fr_l25_form_segments_wrap_in_a_flow_row`(`routines.rs`), `the_settings_sheet_wraps_its_computer_checks_in_a_flow_row`(`project.rs`). | 루틴 폼(새 루틴)과 프로젝트 설정 시트(컴퓨터)를 390과 1440으로: 390에서 프로젝트 선택지가 두 줄 이상이고 모두 보이며, 가로세로 8씩 떨어지고, 눌리거나 잘린 선택지가 없다. 1440에서는 한 줄이다. 44 dp보다 짧은 선택지는 전보다 좁다. |
| `src/ui/cr/home.rs`(`TokenTrendTile`), `src/ui/usage.rs` | 사용량 차트에서 날짜를 고를 수 없었다. | compose-rust#417 | 차트 상자가 `on_pointer`(누름, 이동, 나감, 취소)를 받는다. `usage::trend_day_at`이 날짜를 고르고 차트 아래 줄이 `usage::trend_day_line`을 읽는다. SPEC FR-L23.1. 테스트 `fr_l23_1_the_trend_plot_names_the_day_under_the_pointer`, `fr_l23_1_the_trend_plot_takes_the_pointer_and_reads_the_day_under_it`. | 포인터가 차트 위에 없을 때 홈 대시보드가 1440과 390에서 전과 같다. 포인터로 직접: 막대 위로 옮기면 차트 아래 줄이 그 날의 날짜와 토큰으로 바뀌고, 나가면 합계로 돌아오며, 차트 위에서도 휠로 페이지가 스크롤된다. |
| `src/ui/files.rs`(`FILE_PICKER_AVAILABLE`), `src/ui/cr/files.rs`(트레이의 파일 고르기) | `FILE_PICKER_AVAILABLE`이 false였고 버튼은 파일 단계가 준비 중이라고 했다. | compose-rust#483 | 상수는 핀에서 대화상자가 있는 유일한 플랫폼인 macOS에서 true다. 버튼이 `FileDialog::open_files()`를 열고, `files::pick_outcome`이 답을 보내기 시트의 초안, 변화 없음, 준비 중 안내 중 하나로 바꾼다. 테스트 `fr_l27_5_the_file_picker_follows_the_platform_and_its_answer_fills_the_draft`, `fr_l27_5_the_tray_button_opens_the_platform_file_dialog`. | macOS에서 데모가 아닌 파일 화면을 1440으로: 파일 고르기가 켜져 있고 그 아래 준비 중 줄이 없다. 직접: 버튼이 시스템 패널을 열고, 파일 둘을 고르면 보내기 시트가 열려 둘을 보여 주며, 취소하면 아무것도 바뀌지 않는다. 버튼이 꺼져 있는 Windows와 Linux에서는 확인하지 않았다. |
| `src/ui/cr/components.rs`(`font_refs`), `theme.rs` | 쓰이지 않는 시스템 글꼴 목록이 "글꼴을 넣을 때까지" 남아 있었다. | compose-rust#374, Ember #216 | 없다. 테마가 이미 번들한 Inter, Pretendard, JetBrains Mono 목록을 쓴다(`ui::fonts::app_theme`). `font_refs`와 그 메모를 지웠다. 테스트 `the_font_stack_is_the_themes_and_no_screen_helper_names_one`. | 필요 없다. 그려지는 것이 바뀌지 않는다. 기존 캡처로 글꼴을 확인할 수 있다. |
| `src/ui/cr/project.rs`(`ProjectBoxView`) | 팀 영역이 카드 아래쪽 위에 네 모서리가 모두 둥근 상자였다. | compose-rust#392 | `corner_radii([0, 0, 24, 24])`: 구분선 아래는 각지고 아래쪽은 카드 반지름이다. 테스트 `the_team_area_is_square_under_the_rule_and_round_at_the_cards_bottom`. | 프로젝트 목록을 1440과 390, 라이트와 다크로: `surface-2` 팀 영역이 구분선과 각진 모서리로 만나고, 카드 아래 모서리를 따르며, 테두리 안쪽에 `surface` 틈이 보이지 않는다. |
| `src/ui/cr/project.rs`(`ListActions`, 900 dp 이하의 검색 버튼) | 버튼이 툴킷 글리프를 썼고 이름과 열림 상태가 없었다. | compose-rust#428, #429 | 40 px 고리 안에 Ember의 검색 SVG를 그리고, 이름은 팀원 검색 열기(`a11y::OPEN_MEMBER_SEARCH`), 상태는 `Modifier::expanded`다. 테스트 `fr_l26_the_header_search_button_is_named_and_says_whether_its_row_is_open`. | 프로젝트 목록을 900과 390으로: 40 px 고리에 1.5 px 선 검색 글리프가 18 px, `text-2`로 있고, 다른 헤더 버튼과 크기와 자리가 같다. 직접: VoiceOver가 팀원 검색 열기와 펼침 여부를 읽는다. |

### `feat/673-editor-in-pin`에서 지움, 캡처로 확인할 것

`feat/673-editor-in-pin` 브랜치가 아래 에디터 창 우회 코드를 지운다. 위 표와 같은 이유로 아직
컴파일하지 않았고 화면에서 보지도 않았다.

| 위치 | 우회 코드 | 툴킷 이슈 | 대신 쓰는 것 | 확인할 캡처나 직접 확인 |
|---|---|---|---|---|
| `src/ui/editor_scm.rs`(`diff_editor_view::diff_editor`), `src/ui/editor.rs`(`diff_line_tints`, `with_diff_line_tints`) | 편집 가능한 diff에서 변경이 여백 막대로만 보였다. | compose-rust#542 | 추가되거나 바뀐 줄이 이어진 덩어리마다 `DiffAdded`나 `DiffModified`의 `Decoration::line_background`를 문서 자체 장식 뒤에 둔다. 지운 줄은 여백 점을 그대로 쓴다. 테스트 `fr_l31_10_added_and_changed_lines_are_tinted_in_runs`(`editor.rs`), `fr_l31_10_the_editable_diff_sends_the_line_tints`(`editor_scm.rs`). | 바뀐 파일의 작업 diff를 연 에디터 창을 라이트와 다크로: 추가된 줄은 초록, 바뀐 줄은 수정 역할 색으로 폭 전체와 줄 끝 너머까지 물들고 여백 막대는 전과 같다. 키보드로 직접: 물든 줄에 입력하면 색이 그 줄을 따라간다. |
| `src/ui/editor_scm.rs`(`base_rows`, `diff_view_zones`, `editable_body`, `diff_editor_view::diff_editor`) | 에디터가 맞은편에 빈 행을 둘 수 없어서, 지운 기준 줄이 다음 행 위의 빨간 선과 호버 목록이었다. | compose-rust#543 | 에디터가 지운 덩어리 위 줄 뒤에 지운 줄 수만큼 높은 `view_zones` 영역을 받고, 기준 쪽은 지운 줄마다 제 빨간 행을 그린다(`BaseRow::Removed`). 첫 줄 위에서 지운 줄은 선으로 남는다. 0번 줄 앞에는 영역을 둘 수 없다(아래 이슈 없는 빈 곳). 테스트 `fr_l31_10_removed_lines_take_rows_opposite_view_zones`, `fr_l31_10_the_editable_diff_sends_zones_and_follows_the_pixel_scroll`. | 중간과 끝에서 줄을 지운 작업 diff를 연 에디터 창을 라이트와 다크로: 지운 줄마다 왼쪽에 빨간 행, 오른쪽에 같은 높이의 빈 행이 있고 그 아래 행은 양쪽 높이가 같다. 키보드로 직접: 캐럿이 빈 행을 건너뛴다. |
| `src/ui/editor_scm.rs`(`DiffScroll`, `ScmState::diff_scroll`, `set_diff_scroll`, `set_diff_row_px`) | 기준 쪽이 에디터의 첫 보이는 줄을 따라가서(`on_visible_range_change`, `set_diff_lines`) 줄 단위로 뛰었다. | compose-rust#544 | `on_code_scrolled`와 `on_code_metrics`가 오프셋과 행 높이를 물리 픽셀로 준다. 둘의 비가 위로 넘어간 행 수(소수 포함)이고, 기준 쪽은 그 행부터 그리면서 소수만큼 제 `--code-lh`를 위로 민다. `diff_lines`와 `set_diff_lines`를 지웠다. 기준 쪽에 제 스크롤이 없어서 `set_scroll_offset`은 쓰지 않는다. 테스트 `fr_l31_10_the_base_side_follows_the_editor_by_pixels`, `fr_l31_10_reading_the_same_diff_again_keeps_it_editable`(고침), 위의 연결 테스트. | 긴 작업 diff에서 에디터 위를 트랙패드나 휠로 배율 1과 2에서 직접 스크롤: 기준 행이 에디터 행과 함께 부드럽게 움직이고 줄 중간에서도 높이가 맞는다. |
| `src/ui/editor_language.rs`(`popup_style`, `view::set_caret_rect`, `view::overlay`), `src/ui/editor_workbench.rs`(`editor_pane`) | 목록이 캐럿 옆 팝업이 아니라 창 위의 시트였다. | compose-rust#553 | 에디터의 `on_caret_rect`를 보관하고, 목록이 창 안에서 캐럿 2 px 아래에 열린다. 첫 보고 전에는 시트다. 남은 것: 매개변수 정보는 에디터 영역 아래에 남는다(`TODO(#673)`. `.ed-main` 안에 있는데 페이지가 그 위치를 모르고, #800이 창에 고정한 상자에서 옮겨 왔다). 테스트 `fr_l31_13_a_list_opens_under_the_caret`, `fr_l31_13_the_code_editor_takes_keys_pointer_and_caret`. | 캐럿을 파일 중간, 오른쪽과 아래 가장자리 근처에 두고 `EMBER_EDITOR_LANG_SHEET=completion`과 `actions`를 라이트와 다크로: 목록이 캐럿 줄 아래에 있고 창 안에 머문다. 키보드로 직접: 캐럿을 옮긴 뒤 Ctrl+Space를 누르면 새 위치에 목록이 열린다. |
| `src/ui/editor_language.rs`(`code_key_action`, `view::code_key`), `src/ui/editor_workbench.rs`(`editor_pane`, 코드 메뉴 주석) | 포커스를 가진 코드가 키를 쥐지 않을 때만 언어 단축키가 페이지에 닿았고, 메뉴 메모는 그래서 메뉴가 있다고 했다. | compose-rust#554 | `on_key_preview`가 `code_key_action`(F2와 F12는 이름으로, 나머지는 DOM 키 문자열로)을 돌리고 단축키를 소비한다. 거기서 Escape는 매개변수 정보를 닫는다. 메뉴는 Code-OSS처럼 명령을 그대로 둔다. 남은 것: 목록은 여전히 포커스를 가져가고 제 필드에서 걸러 낸다(`TODO(#673)`). 테스트 `fr_l31_13_the_focused_code_offers_the_chords`와 연결 테스트. | 캐럿을 코드에 두고 키보드로 직접: F12, Shift+F12, F2, Ctrl+Space, Ctrl+Shift+Space, Cmd/Ctrl+.가 각각 기능을 한 번(두 번이 아니라) 열고 아무것도 입력하지 않는다. 보통 입력은 그대로다. |
| `src/ui/editor_language.rs`(`pointer_definition`, `view::code_pointer`), `src/ui/editor_workbench.rs`(`editor_pane`) | Cmd 또는 Ctrl 클릭으로 정의로 가지 못했다. | compose-rust#555 | `pointer_kinds: DOWN`과 `on_code_pointer`: macOS에서는 Cmd, 다른 곳에서는 Ctrl과 함께 글자 위를 주 버튼으로 누르면 그 자리의 `Definition`을 요청하고 소비하므로 캐럿이 그대로다. 테스트 `fr_l31_13_cmd_or_ctrl_click_goes_to_the_definition`. | macOS에서 포인터로 직접: 언어 확장이 있는 파일에서 이름을 Cmd+클릭하면 정의로 간다. 보통 클릭과 끌기는 전처럼 선택하고, Ctrl+클릭은 정의를 열지 않는다. |
| `src/ui/editor.rs`(`DoubleClick`, `DOUBLE_CLICK_MS`), `src/ui/editor_workbench.rs`(`DOUBLE_CLICK`, 트리 행과 탭의 `onclick`) | 탭이나 트리 행을 500 ms 안에 두 번 누르면 두 번 클릭으로 쳤다. | compose-rust#417. dioxus-compose `f0e5b9f5`가 `click_count == 2`인 `Up`에 `dblclick`을 보낸다(`dom/pointer.rs`) | 행과 탭의 `ondoubleclick`이 미리 보기를 고정한다. `onclick`은 열거나 활성화만 한다. 구조체, 상수, 스레드 로컬, 테스트 `fr_l31_tabs_two_clicks_on_one_target_within_500_ms_are_a_double_click`(`tests/editor.rs`)을 지웠다. 테스트 `fr_l31_5_double_click_and_right_click_are_the_real_events`(`editor_workbench_glue_tests.rs`). | 에디터 창에서 포인터로 직접: 파일을 한 번 누르면 기울임 미리 보기 탭이 열리고, 행이나 탭을 두 번 누르면 고정되며, 느린 두 번 클릭은 고정하지 않는다. 터미널 탭을 두 번 누르면 이름 바꾸기가 시작된다. |
| `src/ui/editor_tree_menu.rs`(`PLUS_MENU_HOLDS_SELECTION`, `SELECTION_HEADING`, `TreeMenuEntry::Heading`), `src/ui/editor_workbench.rs`(제목 갈래) | 헤더 "+" 메뉴에 컨텍스트 메뉴 항목을 담은 "선택한 항목" 구역이 있었다. | compose-rust#417. dioxus-compose `f0e5b9f5`가 `contextmenu`를 보낸다 | "+" 메뉴는 새 파일과 새 폴더뿐이다. 행의 `oncontextmenu`가 포인터 위치에 컨텍스트 메뉴를 연다. Shift+F10과 컨텍스트 메뉴 키는 Code-OSS의 키보드 경로라 대역이 아니므로 남는다. 테스트 `design_rule1_plus_menu_is_new_file_and_folder_only`(`design_rule1_plus_menu_is_new_file_and_folder_then_the_stand_in_section`를 대신함). 남은 것: 탭 줄에서 Shift+F10이 여는 ⋯ 메뉴의 현재 탭 묶음(`TODO(#673)`: 키보드로 탭 자체 메뉴를 탭 옆에 연다). | 프로젝트 창의 "+" 메뉴를 1440, 라이트와 다크로: 항목 둘. 포인터로 직접: 트리 행과 탭을 오른쪽 클릭하면 포인터 위치에 각자의 메뉴가 열리고, 가림막을 오른쪽 클릭하면 닫힌다. 키보드로 직접: 트리 행에서 Shift+F10을 누르면 컨텍스트 메뉴가 열린다. |

### 핀 뒤에 닫힘

| 위치 | 우회 코드 | 툴킷 이슈 | 상태 | 다음 핀에서 할 일 |
|---|---|---|---|---|
| `src/ui/cr/components.rs`(`NameThenPill`) | 자식을 제 크기로 두는 weight가 없어, 한 행의 이름과 알약을 `observe_size_every_dp`로 잰다. | compose-rust#778 | 2026-11-14T16:50Z `Modifier::max_height` / `size_in`으로 닫힘. v0.0.11의 `Modifier::weight`는 여전히 몫 전체를 채운다(`fill = false` 미노출). | fill 없는 weight가 노출되면 쓰고 재는 코드를 지운다. |

### 툴킷 이슈 열림

| 위치 | 우회 코드 | 툴킷 이슈 | 상태 | 고쳐지면 할 일 |
|---|---|---|---|---|
| `android/MainActivity.kt:6`, `src/android.rs:59`, `src/ui/back.rs:7` | Android Activity가 Ember의 `back`을 직접 부른다. | compose-rust#302 | 열림 | 툴킷의 BackRequested 이벤트를 받고 JNI 진입점을 지운다. |
| `src/ui/vcs.rs:18`, `src/ui/cr/vcs.rs:2586` | 히스토리는 점이 붙은 목록이고 레인 그래프가 없다. | compose-rust#308 | 열림 | `Canvas`의 Path 명령으로 그래프를 그린다. |
| `src/ui/cr/shell.rs:412` | 하단 바에 배경 흐림이 없다. | compose-rust#300 | 열림 | 흐림과 안전 영역 여백을 넣는다. |
| `src/ui/cr/vcs.rs:344` `StageCheck` | 그린 네모 위에 보이지 않는 툴킷 체크박스를 얹는다. 부분 선택은 선택 안 됨으로 읽힌다. | compose-rust#767 | 열림 | 모양과 부분 상태가 있는 툴킷 `Checkbox`를 쓴다. |
| `src/ui/cr/components.rs:950` | 테두리와 12 px 안쪽 여백이 맨 필드를 감싼 상자에 있다. | compose-rust#771 | 열림 | 필드로 옮긴다. |
| `src/ui/cr/components.rs` (`PopoverMenu`), `home.rs` (`mode_menu`) | 팝오버 메뉴의 방향키와 선택된 행 초기 포커스는 macOS에서 동작한다(2026-11-21 확인, #888). 그러나 키로 옮긴 행에 아무 표시가 없고, 560 이하의 바텀 시트에는 메뉴 키보드가 없다. | compose-rust#810 | 열림(고정된 compose-rust에 키 있음). 표시는 compose-rust#941, 시트는 #940 | 렌더러가 표시를 그리면 다시 확인하고, 툴킷이 시트에서 메뉴 키보드를 주면 쓴다. |
| `src/ui/cr/home.rs` (`model_body`, `ModelRow`) | 홈 모델 선택기의 검색은 평범한 텍스트 필드이고, 행은 이름이 모델인 목록의 목록 항목이며 그려진 활성 행이 선택된 상태다. 목업은 콤보박스, 옵션 목록, `aria-activedescendant`를 쓴다([leader, 2026-11-21], #888). | compose-rust#942 | 열림 | 역할과 관계가 들어오면 검색을 활성 옵션에 묶인 콤보박스로, 행을 옵션으로 바꾼다. |
| `src/ui/cr/home.rs:918` | 모델 팝오버의 행이 `equal_height` 대신 고정 높이를 쓴다. | compose-rust#766 | 열림 | `equal_height`로 되돌린다. |
| `src/ui/cr/project.rs:473` | 목업은 점선인데 추가 칩 테두리가 실선이다. | compose-rust#817 | 열림 | 선 무늬를 준다. |
| `src/ui/cr/project.rs:457` | 스트립의 스크롤 한 걸음을 창 폭에서 구한다. | compose-rust#818 | 열림 | `ScrollState`에서 뷰포트 크기를 읽는다. |
| `src/ui/cr/components.rs`(`EmberCardSized`, `InsetPanelSized`), `src/ui/cr/channel_owner.rs` | `Modifier::padding`에 변마다 다른 여백 빌더가 없고(`PaddingEach`는 와이어에만 있음) `TextField`에 읽기 전용 설정이 없다(`Modifier::min_height`는 v0.0.11에 들어옴). | compose-rust#826 | 열림 | 변별 여백과 `read_only(true)`가 들어오면 쓴다. |
| `src/ui/cr/components.rs`(`DataNamedOverlay`) | 클릭 컨테이너가 `Modifier::label`(`LabelCache`, v0.0.11에서 128개 LRU)로만 이름을 받고 노드별 소유 이름이 없다. | compose-rust#831 | 열림 | 컨테이너가 소유 이름을 받으면 데이터로 이름을 짓는 상자를 `Clickable`로 옮긴다. |
| `src/lib.rs:206`, `src/ide.rs:109`, `ide.rs:167`, `src/ui/coexist.rs:281` | 에디터가 `--editor`로 띄운 별도 프로세스다. | dioxus-compose#161 | 열림. dioxus-compose#152(`Window` 요소)는 닫혔고 핀에 있다. dioxus-compose `a64f3b92`(#161: Compose 루트 아래 `HtmlView` 안의 `WidgetView`에 이벤트가 간다)부터 에디터 창은 페이지를 `HtmlView`로 담은 Compose 루트다(#301, `editor_workbench::view::EditorRoot`). 다만 아직 별도 프로세스다. | 에디터를 셸 호스트의 `Window` 노드로 열고 `editor_command`를 지운다. |
| `src/ui/editor_workbench.rs:3086` | 붙여넣기와 IME 조합 핸들러가 테스트에서는 돌지만 화면에서는 돌지 않는다. | dioxus-compose#205 | 열림 | 터미널의 붙여넣기와 조합을 화면에서 확인하고 메모를 지운다. |

### 핀에 있음, 화면 확인을 기다림

빌더가 화면 하나씩 맡는다. 지우면 그림이나 입력 동작이 바뀌므로 캡처나 실행으로 판단한다.

| 위치 | 우회 코드 | 툴킷 이슈 | 상태 | 할 일 |
|---|---|---|---|---|
| `src/ui/cr/routines.rs`(`ListPane`의 필터 줄) | 프로젝트 필터 칩이 `ScrollRow` 안에서 가로로 스크롤된다. 폭 어림값(`seg_width`)은 지웠다(지운 행 참고). | compose-rust#427, #328 | v0.0.7부터 핀에 있음 | 프로젝트가 여섯 개 이상인 `EMBER_CR_ROUTE=routines`를 1440으로 찍어 여섯 번째 칩이 아직 잘리는지 본다. 잘리지 않으면 이 줄을 `ChoiceRow`로 바꿀 수 있다. |
| `src/ui/cr/vcs.rs`(`ChangesPane`의 `PaneLayout::Two`), `home.rs:328`(`ROW_TILE_H`) | 두 칸이 따로 스크롤된다. 타일 높이가 고정이다. | compose-rust#389 | 핀에 있음(`Row::equal_height`, `ui/widgets.rs`에서 확인). `feat/673-drop-in-pin`에서도 그대로 둔다. `equal_height` 행은 고유 높이로 크기를 정하는데, 줄바꿈되는 글에서는 틀리고 스크롤되는 자식은 0으로 센다(compose-rust#766, 열림). diff 칸은 줄바꿈되는 코드를 담는다. | 두 칸을 페이지 스크롤 하나에 넣고 `equal_height`를 쓴 캡처에서 파일 카드가 diff만큼 높고, diff를 스크롤하는 동안에도 보여야 한다. 홈 타일은 1440과 1024에서 성공 행의 세 타일이 `EmberCardSized`로 같은 높이여야 하고 `ROW_TILE_H`가 없어야 한다. |
| `src/ui/home.rs:1313`(`CHIP_WRAP_WIDTH`) | 홈 입력창이 1100 dp 아래에서 모드 칩, 컴퓨터 칩, 보내기 버튼을 창 폭을 보고 둘째 줄에 둔다. | compose-rust#521 | 핀에 있음(`FlowRow`, `components::ChoiceRow`가 이미 쓴다). `feat/673-drop-in-pin`에서도 그대로 둔다. 이 나눔은 보내기 버튼도 옮기는데, `FlowRow`는 버튼을 줄 끝에 두지 않고 칩과 함께 줄바꿈한다. | 칩을 `FlowRow`에 넣은 입력창을 1440, 1100, 1024, 390으로 찍어 모든 폭에서 보내기 버튼이 마지막 줄 끝에 있어야 한다. |
| `src/terminal_input.rs:145`, `src/ui/editor_workbench.rs` (`terminal_body`, the paste chord) | 에디터 터미널에서 붙여넣기 단축키를 누르면 안내만 뜬다. | compose-rust#546 | 핀에 있음(`compose_rust::clipboard::read_text(callback)`). dioxus-compose의 HTML 런타임도 Cmd+V에 같은 호출을 쓰므로 에디터 창에서도 답이 온다. `feat/673-editor-in-pin`에서도 남겼다: 여러 줄 붙여넣기는 먼저 물어야 하는데(`needs_multiline_warning`, Code-OSS의 대화상자) 창에 그 확인이 없다. 에디터 쪽 표시는 `TODO(#673)`이고, `terminal_input.rs`는 이 브랜치 범위 밖이다. | 여러 줄 확인을 만들고, 단축키에서 클립보드를 읽어 `paste_bytes`를 보낸 뒤 에디터 창에서 확인한다. |
| `src/ui/editor_language.rs` 모듈 메모 | 스니펫이 일반 글자로 들어간다. | compose-rust#556 | 핀에 있음(`CodeEditorHandle::insert_snippet`). 세션이 펼친 글자를 담은 `ember_editor::WidgetCommand::EditCode`를 창에 넘기므로 먼저 `ember/editor`가 스니펫을 실어야 한다. 이 브랜치의 범위 밖이라 표시는 `TODO(#673)`이다. | `WidgetCommand`에 스니펫을 싣고 `insert_snippet`으로 스니펫 세션을 연다. |
| `src/ui/editor_status.rs:110` `list_window` | 고르기 목록을 강조된 행 둘레로 잘라 그린다. | dioxus-compose#200 | 핀에 있음(`MountedData::scroll_to`). `feat/673-editor-in-pin`에서 남겼다: 자른 목록을 지운 빌드에서 JSON 파일의 언어 고르기를 1440으로 찍으니 목록이 맨 위에 있고 강조된 JSON 행이 보이지 않았다. 렌더마다 요청을 보내도 같아서, 이 요청이 이 목록을 움직이지 못한다. 2026-11-22에 코드를 다시 읽었다(#838): dioxus-compose `5b6f707c`와 compose-rust `e52fbd39`에서 보이게 스크롤하는 경로는 바뀌지 않았고, 목록을 만드는 배치에서 `onmounted`가 요청하는 경우, HTML 섬 안의 두 방향 `overflow:auto` 목록의 행을 겨누는 경우를 다루는 툴킷 테스트가 없다. 첫 시도는 강조가 현재 항목으로 옮겨 가기 전의 `onmounted`에서만 요청했다. | 캡처에서 행이 보이면 행을 보이게 스크롤하고 `list_window`를 지운다. |
| `src/ui/cr/components.rs:1882` | 창이 0x0으로 읽히면 첫 구성이 대체값을 쓴다. 에디터 쪽(고정 높이 여유)은 이미 없다: 페이지가 뷰포트를 따른다(`editor::editor_page_size_style`, #301). `feat/673-editor-in-pin`에서 확인했다. | compose-rust#467, #479 | 핀에 있음(`with_report_size`) | 보고된 크기를 따른다. |
| `src/ui/cr/channel_owner.rs:559`, `channels.rs:357`, `home.rs:1508`, `launcher.rs:171`, `new_channel.rs:242`, `new_session.rs:5`, `project.rs:1200`, `project.rs:1712` | 텍스트 필드가 비제어다. | compose-rust#374 | 핀에 있음(`TextField::text`) | 그대로 둔다. 먼저 필드마다 값을 상태로 쥐어야 한다. 필드마다 직접: IME로 조합하면서 한글 단어 중간에 입력한다. 캐럿이 제자리에 있고, 음절이 빠지거나 겹치지 않으며, 코드로 넣은 값(추천, 초기화)이 글을 바꾼다. 캡처로는 볼 수 없다. |
| `src/ui/cr/components.rs:1998`, `channels.rs:627`, `project.rs:371` | 필드 테두리를 맨 필드를 감싼 상자에 그린다. | compose-rust#376, #375 | 핀에 있음 | 그대로 둔다. compose-rust#771(열림) 때문에 12 px 안쪽 여백이 아직 상자에 있다. 테두리를 필드에 둔 검색 필드를 1440과 390, 라이트와 다크로 찍어 1 px `line` 고리, 알약 끝, 높이 가운데의 플레이스홀더가 같아야 한다. |
| `src/ui/cr/components.rs:199`, `components.rs:238` | 모달 목록이 드롭다운 팝업을 대신한다. | compose-rust#383 | 핀에 있음(`Select`, `Dropdown::closed`) | 그대로 둔다. 필드 아래에 열린 툴킷 `Select` 캡처에서 불투명한 `surface` 상자, 1 px `line` 테두리, `corner-md` 반지름, 왼쪽 정렬 `text` 라벨의 38 px 행, `sel` 위의 고른 행이 보이고 앵커 아래로 열려야 한다. |
| `src/ui/cr/theme.rs:341`, `components.rs:1849` | 모서리가 단순 원호다. | compose-rust#294 | 핀에 있음(`Modifier::corner_curve`) | 그대로 둔다. 이 값은 모든 카드, 시트, 둥근 네모에 한꺼번에 들어간다. `corner_curve(0.6)`을 준 홈, 프로젝트, 세션 화면과 시트를 1440, 두 테마로 찍어, 목업이 `corner-shape: squircle`을 준 곳의 모서리와 같고 테두리와 그림자가 같은 곡선을 따르며 잘린 자식에 틈이 없어야 한다. |
| `src/ui/cr/session.rs:5170`, `components.rs:1806` | Escape로 서랍이 닫히지 않는다. 모달은 닫기 콜백을 직접 넘긴다. | compose-rust#476 | 핀에 있음(`on_key_down`, `request_focus`) | 그대로 둔다. 키보드로 직접: 서랍을 열고 Escape를 누르면 서랍이 닫히고 포커스가 서랍을 연 버튼으로 돌아간다. 모달 시트도 같다. 캡처로는 볼 수 없다. |
| `src/ui/cr/session.rs:1285` | 터미널 행이 `SelectionContainer` 안에 없다. | compose-rust#5 | 핀에 있음 | 그대로 둔다. 포인터로 직접: 터미널 행 여러 개를 끌어 선택한다. 선택이 꾸민 행의 글자를 따르고, 줄바꿈과 함께 읽는 순서로 복사되며, 행의 컨트롤은 여전히 눌린다. |
| `src/ui/cr/home.rs:438`, `session.rs:3794`, `project.rs:1347`, `files.rs:506`, `characters_screen.rs:200`, `conversation.rs:257` | `EMBER_CR_SCROLL`이 검토 캡처용으로 블록을 뺀다. | compose-rust#385 | 핀에 있음(`ScrollState::scroll_to`) | 그대로 둔다. 검토 캡처 스크립트가 `EMBER_CR_SCROLL`을 읽는다. 스크립트가 실제로 스크롤하면 지운다. 그때 여섯 화면 아래쪽 절반의 캡처가 블록을 뺀 캡처와 같아야 한다. |
| `src/ui/cr/home.rs:2087` | 줄무늬 높이가 고정이다. | compose-rust#388 | 핀에 있음(`Canvas::view_box`) | 그대로 둔다. 줄무늬를 블록을 채우는 `view_box` 캔버스에 그린 홈 승인 대기 타일을 1440, 1024, 390으로 찍어, 줄무늬가 모든 블록의 양끝에 닿고 각도와 간격이 같아야 한다. |
| `src/ui/vcs.rs:16` | 텍스트 Button이 행이나 diff 줄의 누르는 대상이다. | compose-rust#372 | 핀에 있음(컨테이너 `on_click`) | VCS 행을 옮긴다. |

### 툴킷 이슈 없는 Ember 일

| 위치 | 메모 | 할 일 |
|---|---|---|
| `src/ui/term_pane.rs:15` | 한 번에 500줄을 그린다. 툴킷에 `LazyColumn`이 있다(`compose-rust/src/ui/lazy.rs`). | 줄을 `LazyColumn`에 그리고 화면에서 확인한다. |
| `src/ui/cr/compat.rs:146` `CodeText` | 긴 줄이 줄바꿈된다. 툴킷에 `ScrollRow`가 있다. | 글을 `ScrollRow`에 넣고 화면에서 확인한다. |

### 이슈 없는 툴킷 빈 곳

저장한 목록에 compose-rust 이슈가 없다. 리더가 이슈를 올리면 표시에 번호를 적는다. (변별 여백, 최소
높이, 읽기 전용 `TextField`는 `compose-rust#826`으로, 클릭 컨테이너의 소유 접근성 이름은
`compose-rust#831`로 올라갔고 `Modifier::min_height`는 v0.0.11에 들어왔다.)

| 위치 | 빈 곳 | 툴킷 소스 |
|---|---|---|
| `src/ui/editor_scm.rs`(`editable_body`) | 코드 에디터의 view zone은 줄 뒤에 붙으므로(`ViewZone::after_line`) 0번 줄 위에는 둘 수 없다. diff의 첫 줄 위에서 지운 기준 줄은 첫 행의 빨간 선과 호버 목록으로 남는다. | `compose-rust/src/code.rs`(`ViewZone`), `renderer/desktop/src/renderer/CodeEditorSurface.kt:591` |

### 이번에 바꾸지 않은 메모

`src/ui/project.rs:12`, `src/ui/session.rs:14`, `src/ui/characters.rs:9`는 예전 Dioxus 화면의
빈 곳을 번호 없이 적는다("no clickable Row", "no back stack", "no animation"). compose-rust
화면이 아니라 모델 파일의 설명이다. 맞는 툴킷 이슈는 compose-rust#372(핀에 있음), #302(열림),
애니메이션 API(핀에 있음)다. 이 파일들을 다음에 고칠 때까지 그대로 둔다.
