# TOOLKIT-WORKAROUNDS.md: Ember workarounds for toolkit gaps (#673)

Status: **marker pass done on 2026-11-15, not compiled.** The table is read from the source of
`ember/app/` on branch `feat/673-marker-pass`. Line numbers are that branch's. No row here was
checked on a display. Branch `feat/673-drop-in-pin` then removes the base screens' workarounds
whose fix is in the pin; those rows are in their own table and name functions, not lines.

Korean follows the English section with the same content.

## English

### Rule

Every Ember workaround for a toolkit bug or gap cites a toolkit issue, and goes once the fix is
in the pinned toolkit, because a workaround without an issue is never removed. The pins are
compose-rust tag `v0.0.10` (commit `3df7c17`, published 2026-11-14T09:44Z) and dioxus-compose
rev `f0e5b9f5` (committed 2026-11-14T10:00Z, built on compose-rust v0.0.10).

A marker has one of five forms, so that one grep finds each group at the next pin:

| Form | Meaning | At the next pin |
|---|---|---|
| `TODO(compose-rust#N)`, `TODO(dioxus-compose#N)` | The toolkit issue is open. | Read the issue state; when it is closed, check the API in the new pin and move the marker. |
| `TODO(compose-rust#N, after v0.0.10)`, `TODO(dioxus-compose#N, after f0e5b9f5)` | The issue was closed after the pin and the API is not in the pinned source. | Confirm the API in the new pin, remove the workaround with a test, or move it to the display list. |
| `TODO(compose-rust#N, in v0.0.10)`, `TODO(dioxus-compose#N, in f0e5b9f5)` | The API is in the pinned source. Removing the workaround changes what is drawn or how input behaves, so it waits for a builder with a display. | Nothing waits for a pin. A builder removes it and checks it on a display. |
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

The counts above are the marker pass's; `feat/673-drop-in-pin` has not recounted them.

The first grep of this pass found 186 lines that name a toolkit issue. The rest of them are
references to a fix that is already in use (for example `compose-rust#372` beside a container
`on_click`) and tests that guard against a removed marker coming back. They need no action.

### Removed in this pass

| Place | Workaround | Toolkit issue | State | What replaced it |
|---|---|---|---|---|
| `src/ui/cr/session.rs` `PaneTerminal` | The terminal pane opened at its top and did not follow new output. | compose-rust#385 | Closed, in the pin since v0.0.5 (`ScrollState::scroll_to`) | The pane's `ScrollColumn` takes `remember_scroll_state()` and `FollowNewest` moves it, as the transcript does. Test `pane_terminal_follows_its_newest_line_with_the_toolkit_scroll_state`. |
| `src/ui/cr/compat.rs` `SplitPane` | A fixed two-column row stood in for a split pane. It had no caller. | compose-rust#27 | Closed, in the pin (`compose_rust::ui::SplitPane`) | The function is gone. Test `no_stand_in_split_pane_is_left`. |

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

### Closed after the pin

| Place | Workaround | Toolkit issue | State | Action at the next pin |
|---|---|---|---|---|
| `src/ui/cr/channel_owner.rs:381`, `add_computer.rs:423`, `files.rs:1584`, `project.rs:1157`, `project.rs:1384` | A sheet body takes a fixed or estimated height, because a node's height cannot be capped. | compose-rust#778 | Closed 2026-11-14T16:50Z. No maximum height on `Modifier` in the pin (`compose-rust/src/ui/mod.rs`). | Cap the body and delete the estimates (`sheet_body_max_height` callers, `new_project_body_estimate`, `add_body_estimate`). |
| `src/ui/cr/vcs.rs:801`, `vcs.rs:1534` | The branch list takes a fixed 300 and the changes tree a fixed cap. | compose-rust#778 | As above | Cap both and delete `branch_pop_list_height` and the fixed height in `tree_cap`. |
| `src/ui/cr/components.rs:1202` | A name and a pill in a row are measured through `observe_size_every_dp`, because no weight leaves a child its own size. | compose-rust#778 | As above (the title also names weight without fill) | Use the weight without fill and delete the measuring. |
| `src/ui/cr/vcs.rs:508`, `session.rs:558` | A press area is as large as its row at most; the IDE chevron is 26 by 32. | compose-rust#472 | Closed 2026-11-14T22:03Z. No hit area API in the pin. | Give both a 44 by 44 hit area that does not grow the node. |
| `src/ui/vcs.rs:17` | The diff line check scrolls with its row. | compose-rust#424 | Closed 2026-11-14T17:07Z. No sticky column in the pin. | Pin the check column. |
| `src/ui/cr/components.rs:1049` `OneLineHintField` | Ember draws the hint as its own one-line label under the field. | compose-rust#786 | Closed 2026-11-14T22:03Z. `TextField` has no placeholder line limit in the pin. | Delete `OneLineHintField` and pass the hint as the placeholder. |
| `assets/editor/page.css:36` | `.ed-layers` and `.ed-none` keep placeholder comment nodes out of the grid rows. | dioxus-compose#266 | Closed 2026-11-14T22:03Z, after the pinned commit. | Drop both rules and check the status bar's place. |

### Toolkit issue open

| Place | Workaround | Toolkit issue | State | Action when it is fixed |
|---|---|---|---|---|
| `android/MainActivity.kt:6`, `src/android.rs:59`, `src/ui/back.rs:7` | The Android Activity calls Ember's `back` itself. | compose-rust#302 | Open | Take the toolkit's BackRequested event and delete the JNI entry. |
| `src/ui/vcs.rs:18`, `src/ui/cr/vcs.rs:2586` | The history is a list with a dot, with no lane graph. | compose-rust#308 | Open | Draw the graph on a `Canvas` with Path commands. |
| `src/ui/cr/shell.rs:412` | The bottom bar has no backdrop blur. | compose-rust#300 | Open | Add the blur and the safe-area inset. |
| `src/ui/cr/vcs.rs:344` `StageCheck` | A painted square lies over an invisible toolkit checkbox; a partial check reports unchecked. | compose-rust#767 | Open | Use the toolkit `Checkbox` with its shape and partial state. |
| `src/ui/cr/components.rs:950` | The frame and the 12 px inset sit on a box around a bare field. | compose-rust#771 | Open | Move them onto the field. |
| `src/ui/cr/components.rs:482`, `home.rs:826` | A popover menu has no arrow keys and no focus on the current row. | compose-rust#810 | Open | Add the keys and the focus from the toolkit. |
| `src/ui/cr/home.rs:918` | The model popover's row takes a fixed height, not `equal_height`. | compose-rust#766 | Open | Use `equal_height` again. |
| `src/ui/cr/project.rs:473` | The add chip has a solid border where the mockup has a dashed one. | compose-rust#817 | Open | Set the stroke dash. |
| `src/ui/cr/project.rs:457` | The strip's scroll step is taken from the window's width. | compose-rust#818 | Open | Read the viewport size from `ScrollState`. |
| `src/lib.rs:206`, `lib.rs:219`, `src/ide.rs:109`, `ide.rs:167`, `src/ui/coexist.rs:281` | The editor is its own process started with `--editor`. | dioxus-compose#161 | Open. dioxus-compose#152 (the `Window` element) is closed and in the pin; the shell is a Compose root and the editor an HTML page. | Open the editor as a `Window` node on the shell's host and delete `editor_command`. |
| `src/ui/editor_workbench.rs:3086` | Paste and IME composition handlers run in tests but not on screen. | dioxus-compose#205 | Open | Check the terminal's paste and composition on a display, then drop the note. |

### In the pin, waits for a display check

A builder takes these one screen at a time. Each removal changes what is drawn or how input
behaves, so a capture or a run decides it.

| Place | Workaround | Toolkit issue | State | Action |
|---|---|---|---|---|
| `src/ui/cr/routines.rs` (`ListPane`, the filter row) | The project filter chips scroll sideways in a `ScrollRow`. Their width estimate (`seg_width`) is gone, see the removed rows. | compose-rust#427, #328 | In the pin since v0.0.7 | A capture of `EMBER_CR_ROUTE=routines` at 1440 with six or more projects must show whether the sixth chip still clips; if it does not, the row can become a `ChoiceRow`. |
| `src/ui/cr/vcs.rs` (`ChangesPane`, `PaneLayout::Two`), `home.rs:328` (`ROW_TILE_H`) | Panes scroll on their own; tiles take a fixed height. | compose-rust#389 | In the pin (`Row::equal_height`, confirmed in `ui/widgets.rs`). Kept on `feat/673-drop-in-pin`: an `equal_height` row is sized by intrinsic heights, which is wrong for wrapped text and counts a scrolling child as 0 (compose-rust#766, open), and the diff pane holds wrapped code. | A capture of the two panes in one page scroll with `equal_height` must show the files card as tall as the diff and still in view while the diff scrolls. For the home tiles: the three tiles of the success row at 1440 and 1024 must be equally tall with `EmberCardSized` and no `ROW_TILE_H`. |
| `src/ui/home.rs:1313` (`CHIP_WRAP_WIDTH`) | The Home composer puts its mode and computer chips and the send button on a second line below 1100 dp, by window width. | compose-rust#521 | In the pin (`FlowRow`, already used by `components::ChoiceRow`). Kept on `feat/673-drop-in-pin`: the split also moves the send button, which a `FlowRow` would wrap with the chips instead of keeping at the row's end. | A capture of the composer at 1440, 1100, 1024 and 390 with the chips in a `FlowRow` must show the send button at the end of the last line at every width. |
| `src/terminal_input.rs:145`, `src/ui/editor_workbench.rs:3070` | A paste chord in the editor terminal shows a notice. | compose-rust#546 | In the pin (`compose_rust::clipboard::read_text(callback)`, confirmed in `clipboard.rs`). Not touched on `feat/673-drop-in-pin`: the chord is handled in the editor window's files. | Read the clipboard on the chord and check it in the editor window. |
| `src/ui/editor_scm.rs:2227` | A change shows as gutter bars only. | compose-rust#542 | In the pin (`Decoration::line_background`, forwarded by dioxus-compose) | Tint the added and changed lines. |
| `src/ui/editor_scm.rs:1629`, `editor_scm.rs:1643` | Removed lines are a red rule with a hover list. | compose-rust#543 | In the pin (`CodeEditor` `view_zones`) | Give removed lines rows of their own. |
| `src/ui/editor_scm.rs:1596`, `editor_scm.rs:2274` | The diff's base side follows whole lines and assumes 20 px rows. | compose-rust#544 | In the pin (`on_code_metrics`, `on_code_scrolled`, `set_scroll_offset`) | Sync by pixels. |
| `src/ui/editor_language.rs:20`, `:645`, `:679` | Lists and hints are a sheet and a bottom strip, not popups at the caret. | compose-rust#553 | In the pin (`on_caret_rect`, `query_position_rect`) | Anchor them at the caret. |
| `src/ui/editor_language.rs:22`, `:53`, `:680`, `src/ui/editor_workbench.rs:1691` | The editor menu carries every language command, and a list takes the focus. | compose-rust#554 | In the pin (`on_key_preview`) | Take the keys from the editor and trim the menu. |
| `src/ui/editor_language.rs:25`, `:503` | No Cmd or Ctrl click to a definition. | compose-rust#555 | In the pin (`on_code_pointer`, `pointer_kinds`) | Start `Definition` at the pointer. |
| `src/ui/editor_language.rs:27` | A snippet is inserted as plain text. | compose-rust#556 | In the pin (`CodeEditorHandle::insert_snippet`) | Open a snippet session. |
| `src/ui/editor_status.rs:110` `list_window` | A picker list is windowed around the highlighted row. | dioxus-compose#200 | In the pin (`MountedData::scroll_to`) | Scroll the row into view and delete `list_window`. |
| `src/ui/editor_tree_menu.rs:9`, `:124`, `src/ui/editor_workbench.rs:110`, `:1625`, `:2166`, `:2478`, `:2552`, `:2651`, `:2993`, `src/ui/editor.rs:620`, `:640` | Right-click and double-click have stand-ins: a section of the "+" menu, Shift+F10, the tab menu, two clicks within 500 ms. | compose-rust#417, #100, #717 | In the pin; dioxus-compose `f0e5b9f5` sends `contextmenu`, `dblclick`, F1 to F12 and the context-menu key (`dom/pointer.rs`) | Check right-click and double-click in the editor window, then delete `PLUS_MENU_HOLDS_SELECTION`, `DoubleClick` and the stand-in menu items. |
| `src/ui/editor_notifications.rs:18`, `src/ui/editor_workbench.rs:1396`, `src/ui/cr/components.rs:1787` | The editor page keeps a fixed height allowance; the first composition falls back when the window reads 0x0. | compose-rust#467, #479 | In the pin (`with_report_size`) | Follow the reported size. |
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
the number.

| Place | Gap | Toolkit source |
|---|---|---|
| `src/ui/cr/components.rs:1673` (`EmberCardSized`), `:1702` (`InsetPanelSized`) | `ui::Modifier` has no per-side padding. `Modifier::padding(dp)` is uniform and `padding_role` takes one role. The wire has `Modifier::PaddingEach`, with no builder. Ember adds 4 dp spacers to turn 16 into 16/20 and 12 into 12/16. | `compose-rust/src/ui/mod.rs:500`, `compose-rust/src/schema.rs:2421` |
| `src/ui/cr/agents.rs:854` (`HeadedCard`) | `ui::Modifier` has no minimum height. Ember puts a zero-width spacer of the height in the row as a strut. compose-rust#778 is about a maximum by its title; whether its fix also gives a minimum is not known from the pin. | `compose-rust/src/ui/mod.rs` (`width`, `height`, `size` only) |
| `src/ui/cr/components.rs` (`DataNamedOverlay`) | A clickable container has no owned accessible name. `Modifier::label` keeps every distinct name for the life of the program and drops names past 4096, so it suits a fixed set only. A `Button` with content (compose-rust#425) insets its children by the design system's 16 dp and gives no way to change it, so a painted row cannot move into one. A box named from data therefore keeps a clear `Button` over it. | `compose-rust/src/ui/mod.rs` (`keep_label`, `MAX_LABELS`), `renderer/desktop/src/renderer/HostButton.kt` (`labelPadding`); filed as compose-rust#831 |
| `src/ui/cr/channel_owner.rs:467` | `TextField` has no read-only flag. The invite link is shown in an enabled field so it can be selected and copied, and typing changes what is drawn. | `compose-rust/src/ui/widgets.rs` (`TextFieldScope`, line 1163) |

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
`v0.0.10`(커밋 `3df7c17`, 2026-11-14T09:44Z 배포)과 dioxus-compose 리비전 `f0e5b9f5`
(2026-11-14T10:00Z 커밋, compose-rust v0.0.10 기준)이다.

표시는 다섯 가지 꼴 중 하나다. 다음 핀 때 grep 한 번으로 묶음을 찾기 위해서다.

| 꼴 | 뜻 | 다음 핀에서 할 일 |
|---|---|---|
| `TODO(compose-rust#N)`, `TODO(dioxus-compose#N)` | 툴킷 이슈가 열려 있다. | 이슈 상태를 읽는다. 닫혔으면 새 핀에서 API를 확인하고 표시를 옮긴다. |
| `TODO(compose-rust#N, after v0.0.10)`, `TODO(dioxus-compose#N, after f0e5b9f5)` | 이슈가 핀 뒤에 닫혔고 핀 소스에 API가 없다. | 새 핀에서 API를 확인하고 테스트와 함께 우회 코드를 지우거나, 화면 확인 목록으로 옮긴다. |
| `TODO(compose-rust#N, in v0.0.10)`, `TODO(dioxus-compose#N, in f0e5b9f5)` | API가 핀 소스에 있다. 지우면 그림이나 입력 동작이 바뀌므로 화면을 볼 수 있는 빌더를 기다린다. | 핀을 기다리지 않는다. 빌더가 지우고 화면에서 확인한다. |
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

위 개수는 표시 작업 때의 값이다. `feat/673-drop-in-pin`에서는 다시 세지 않았다.

이번 작업의 첫 grep은 툴킷 이슈를 적은 줄 186개를 찾았다. 나머지는 이미 쓰고 있는 수정을
가리키는 참조(예: 컨테이너 `on_click` 옆의 `compose-rust#372`)와, 지운 표시가 돌아오지 않게
막는 테스트다. 할 일이 없다.

### 이번에 지움

| 위치 | 우회 코드 | 툴킷 이슈 | 상태 | 대신 쓰는 것 |
|---|---|---|---|---|
| `src/ui/cr/session.rs` `PaneTerminal` | 터미널 칸이 맨 위에서 열리고 새 출력을 따라가지 않았다. | compose-rust#385 | 닫힘, v0.0.5부터 핀에 있음(`ScrollState::scroll_to`) | 칸의 `ScrollColumn`이 `remember_scroll_state()`를 받고, 대화와 같이 `FollowNewest`가 옮긴다. 테스트 `pane_terminal_follows_its_newest_line_with_the_toolkit_scroll_state`. |
| `src/ui/cr/compat.rs` `SplitPane` | 고정 두 칸 행이 분할 창을 대신했다. 부르는 곳이 없었다. | compose-rust#27 | 닫힘, 핀에 있음(`compose_rust::ui::SplitPane`) | 함수를 지웠다. 테스트 `no_stand_in_split_pane_is_left`. |

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

### 핀 뒤에 닫힘

| 위치 | 우회 코드 | 툴킷 이슈 | 상태 | 다음 핀에서 할 일 |
|---|---|---|---|---|
| `src/ui/cr/channel_owner.rs:381`, `add_computer.rs:423`, `files.rs:1584`, `project.rs:1157`, `project.rs:1384` | 노드 높이에 상한을 둘 수 없어 시트 본문 높이를 고정하거나 어림한다. | compose-rust#778 | 2026-11-14T16:50Z 닫힘. 핀의 `Modifier`에 최대 높이가 없다(`compose-rust/src/ui/mod.rs`). | 본문에 상한을 두고 어림 함수를 지운다(`sheet_body_max_height`를 부르는 곳, `new_project_body_estimate`, `add_body_estimate`). |
| `src/ui/cr/vcs.rs:801`, `vcs.rs:1534` | 브랜치 목록은 300으로, 변경 트리는 정한 높이로 고정한다. | compose-rust#778 | 위와 같음 | 둘 다 상한으로 바꾸고 `branch_pop_list_height`와 `tree_cap`의 고정 높이를 지운다. |
| `src/ui/cr/components.rs:1202` | 자식을 제 크기로 두는 weight가 없어, 한 행의 이름과 알약을 `observe_size_every_dp`로 잰다. | compose-rust#778 | 위와 같음(제목에 fill 없는 weight도 있다) | fill 없는 weight를 쓰고 재는 코드를 지운다. |
| `src/ui/cr/vcs.rs:508`, `session.rs:558` | 누르는 영역이 행 높이를 넘지 못한다. IDE 화살표는 26x32다. | compose-rust#472 | 2026-11-14T22:03Z 닫힘. 핀에 누르는 영역 API가 없다. | 둘 다 노드를 키우지 않는 44x44 영역을 준다. |
| `src/ui/vcs.rs:17` | diff 줄의 체크가 행과 함께 가로로 스크롤된다. | compose-rust#424 | 2026-11-14T17:07Z 닫힘. 핀에 고정 열이 없다. | 체크 열을 고정한다. |
| `src/ui/cr/components.rs:1049` `OneLineHintField` | 힌트를 Ember가 필드 아래에 한 줄 라벨로 그린다. | compose-rust#786 | 2026-11-14T22:03Z 닫힘. 핀의 `TextField`에 플레이스홀더 줄 제한이 없다. | `OneLineHintField`를 지우고 힌트를 플레이스홀더로 넘긴다. |
| `assets/editor/page.css:36` | `.ed-layers`와 `.ed-none`이 자리 표시 주석 노드를 그리드 행에서 뺀다. | dioxus-compose#266 | 2026-11-14T22:03Z 닫힘, 핀 커밋 뒤. | 두 규칙을 지우고 상태 표시줄 위치를 확인한다. |

### 툴킷 이슈 열림

| 위치 | 우회 코드 | 툴킷 이슈 | 상태 | 고쳐지면 할 일 |
|---|---|---|---|---|
| `android/MainActivity.kt:6`, `src/android.rs:59`, `src/ui/back.rs:7` | Android Activity가 Ember의 `back`을 직접 부른다. | compose-rust#302 | 열림 | 툴킷의 BackRequested 이벤트를 받고 JNI 진입점을 지운다. |
| `src/ui/vcs.rs:18`, `src/ui/cr/vcs.rs:2586` | 히스토리는 점이 붙은 목록이고 레인 그래프가 없다. | compose-rust#308 | 열림 | `Canvas`의 Path 명령으로 그래프를 그린다. |
| `src/ui/cr/shell.rs:412` | 하단 바에 배경 흐림이 없다. | compose-rust#300 | 열림 | 흐림과 안전 영역 여백을 넣는다. |
| `src/ui/cr/vcs.rs:344` `StageCheck` | 그린 네모 위에 보이지 않는 툴킷 체크박스를 얹는다. 부분 선택은 선택 안 됨으로 읽힌다. | compose-rust#767 | 열림 | 모양과 부분 상태가 있는 툴킷 `Checkbox`를 쓴다. |
| `src/ui/cr/components.rs:950` | 테두리와 12 px 안쪽 여백이 맨 필드를 감싼 상자에 있다. | compose-rust#771 | 열림 | 필드로 옮긴다. |
| `src/ui/cr/components.rs:482`, `home.rs:826` | 팝오버 메뉴에 방향키 이동과 현재 행 포커스가 없다. | compose-rust#810 | 열림 | 툴킷의 키와 포커스를 붙인다. |
| `src/ui/cr/home.rs:918` | 모델 팝오버의 행이 `equal_height` 대신 고정 높이를 쓴다. | compose-rust#766 | 열림 | `equal_height`로 되돌린다. |
| `src/ui/cr/project.rs:473` | 목업은 점선인데 추가 칩 테두리가 실선이다. | compose-rust#817 | 열림 | 선 무늬를 준다. |
| `src/ui/cr/project.rs:457` | 스트립의 스크롤 한 걸음을 창 폭에서 구한다. | compose-rust#818 | 열림 | `ScrollState`에서 뷰포트 크기를 읽는다. |
| `src/lib.rs:206`, `lib.rs:219`, `src/ide.rs:109`, `ide.rs:167`, `src/ui/coexist.rs:281` | 에디터가 `--editor`로 띄운 별도 프로세스다. | dioxus-compose#161 | 열림. dioxus-compose#152(`Window` 요소)는 닫혔고 핀에 있다. 셸은 Compose 루트이고 에디터는 HTML 페이지다. | 에디터를 셸 호스트의 `Window` 노드로 열고 `editor_command`를 지운다. |
| `src/ui/editor_workbench.rs:3086` | 붙여넣기와 IME 조합 핸들러가 테스트에서는 돌지만 화면에서는 돌지 않는다. | dioxus-compose#205 | 열림 | 터미널의 붙여넣기와 조합을 화면에서 확인하고 메모를 지운다. |

### 핀에 있음, 화면 확인을 기다림

빌더가 화면 하나씩 맡는다. 지우면 그림이나 입력 동작이 바뀌므로 캡처나 실행으로 판단한다.

| 위치 | 우회 코드 | 툴킷 이슈 | 상태 | 할 일 |
|---|---|---|---|---|
| `src/ui/cr/routines.rs`(`ListPane`의 필터 줄) | 프로젝트 필터 칩이 `ScrollRow` 안에서 가로로 스크롤된다. 폭 어림값(`seg_width`)은 지웠다(지운 행 참고). | compose-rust#427, #328 | v0.0.7부터 핀에 있음 | 프로젝트가 여섯 개 이상인 `EMBER_CR_ROUTE=routines`를 1440으로 찍어 여섯 번째 칩이 아직 잘리는지 본다. 잘리지 않으면 이 줄을 `ChoiceRow`로 바꿀 수 있다. |
| `src/ui/cr/vcs.rs`(`ChangesPane`의 `PaneLayout::Two`), `home.rs:328`(`ROW_TILE_H`) | 두 칸이 따로 스크롤된다. 타일 높이가 고정이다. | compose-rust#389 | 핀에 있음(`Row::equal_height`, `ui/widgets.rs`에서 확인). `feat/673-drop-in-pin`에서도 그대로 둔다. `equal_height` 행은 고유 높이로 크기를 정하는데, 줄바꿈되는 글에서는 틀리고 스크롤되는 자식은 0으로 센다(compose-rust#766, 열림). diff 칸은 줄바꿈되는 코드를 담는다. | 두 칸을 페이지 스크롤 하나에 넣고 `equal_height`를 쓴 캡처에서 파일 카드가 diff만큼 높고, diff를 스크롤하는 동안에도 보여야 한다. 홈 타일은 1440과 1024에서 성공 행의 세 타일이 `EmberCardSized`로 같은 높이여야 하고 `ROW_TILE_H`가 없어야 한다. |
| `src/ui/home.rs:1313`(`CHIP_WRAP_WIDTH`) | 홈 입력창이 1100 dp 아래에서 모드 칩, 컴퓨터 칩, 보내기 버튼을 창 폭을 보고 둘째 줄에 둔다. | compose-rust#521 | 핀에 있음(`FlowRow`, `components::ChoiceRow`가 이미 쓴다). `feat/673-drop-in-pin`에서도 그대로 둔다. 이 나눔은 보내기 버튼도 옮기는데, `FlowRow`는 버튼을 줄 끝에 두지 않고 칩과 함께 줄바꿈한다. | 칩을 `FlowRow`에 넣은 입력창을 1440, 1100, 1024, 390으로 찍어 모든 폭에서 보내기 버튼이 마지막 줄 끝에 있어야 한다. |
| `src/terminal_input.rs:145`, `src/ui/editor_workbench.rs:3070` | 에디터 터미널에서 붙여넣기 단축키를 누르면 안내만 뜬다. | compose-rust#546 | 핀에 있음(`compose_rust::clipboard::read_text(callback)`, `clipboard.rs`에서 확인). `feat/673-drop-in-pin`에서는 건드리지 않았다. 단축키는 에디터 창 파일에서 처리한다. | 단축키에서 클립보드를 읽고 에디터 창에서 확인한다. |
| `src/ui/editor_scm.rs:2227` | 변경이 여백 막대로만 보인다. | compose-rust#542 | 핀에 있음(`Decoration::line_background`, dioxus-compose가 전달) | 추가되고 바뀐 줄에 바탕색을 준다. |
| `src/ui/editor_scm.rs:1629`, `editor_scm.rs:1643` | 지운 줄이 빨간 선과 호버 목록이다. | compose-rust#543 | 핀에 있음(`CodeEditor`의 `view_zones`) | 지운 줄에 제 행을 준다. |
| `src/ui/editor_scm.rs:1596`, `editor_scm.rs:2274` | diff의 기준 쪽이 줄 단위로 따라가고 행 높이를 20 px로 가정한다. | compose-rust#544 | 핀에 있음(`on_code_metrics`, `on_code_scrolled`, `set_scroll_offset`) | 픽셀 단위로 맞춘다. |
| `src/ui/editor_language.rs:20`, `:645`, `:679` | 목록과 힌트가 캐럿 옆 팝업이 아니라 시트와 아래 띠다. | compose-rust#553 | 핀에 있음(`on_caret_rect`, `query_position_rect`) | 캐럿에 붙인다. |
| `src/ui/editor_language.rs:22`, `:53`, `:680`, `src/ui/editor_workbench.rs:1691` | 에디터 메뉴가 언어 명령을 모두 담고, 목록이 포커스를 가져간다. | compose-rust#554 | 핀에 있음(`on_key_preview`) | 에디터에서 키를 받고 메뉴를 줄인다. |
| `src/ui/editor_language.rs:25`, `:503` | Cmd 또는 Ctrl 클릭으로 정의로 가지 못한다. | compose-rust#555 | 핀에 있음(`on_code_pointer`, `pointer_kinds`) | 포인터 위치에서 `Definition`을 시작한다. |
| `src/ui/editor_language.rs:27` | 스니펫이 일반 글자로 들어간다. | compose-rust#556 | 핀에 있음(`CodeEditorHandle::insert_snippet`) | 스니펫 세션을 연다. |
| `src/ui/editor_status.rs:110` `list_window` | 고르기 목록을 강조된 행 둘레로 잘라 그린다. | dioxus-compose#200 | 핀에 있음(`MountedData::scroll_to`) | 행을 보이게 스크롤하고 `list_window`를 지운다. |
| `src/ui/editor_tree_menu.rs:9`, `:124`, `src/ui/editor_workbench.rs:110`, `:1625`, `:2166`, `:2478`, `:2552`, `:2651`, `:2993`, `src/ui/editor.rs:620`, `:640` | 오른쪽 클릭과 두 번 클릭에 대역이 있다: "+" 메뉴의 한 구역, Shift+F10, 탭 메뉴, 500 ms 안의 두 번 클릭. | compose-rust#417, #100, #717 | 핀에 있음. dioxus-compose `f0e5b9f5`가 `contextmenu`, `dblclick`, F1부터 F12, 컨텍스트 메뉴 키를 보낸다(`dom/pointer.rs`) | 에디터 창에서 오른쪽 클릭과 두 번 클릭을 확인한 뒤 `PLUS_MENU_HOLDS_SELECTION`, `DoubleClick`, 대역 메뉴 항목을 지운다. |
| `src/ui/editor_notifications.rs:18`, `src/ui/editor_workbench.rs:1396`, `src/ui/cr/components.rs:1787` | 에디터 페이지가 고정 높이 여유를 두고, 창이 0x0으로 읽히면 첫 구성이 대체값을 쓴다. | compose-rust#467, #479 | 핀에 있음(`with_report_size`) | 보고된 크기를 따른다. |
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

저장한 목록에 compose-rust 이슈가 없다. 리더가 이슈를 올리면 표시에 번호를 적는다.

| 위치 | 빈 곳 | 툴킷 소스 |
|---|---|---|
| `src/ui/cr/components.rs:1673`(`EmberCardSized`), `:1702`(`InsetPanelSized`) | `ui::Modifier`에 변마다 다른 안쪽 여백이 없다. `Modifier::padding(dp)`는 네 변이 같고 `padding_role`은 역할 하나를 받는다. 와이어에는 `Modifier::PaddingEach`가 있지만 빌더가 없다. Ember는 4 dp 스페이서를 넣어 16을 16/20으로, 12를 12/16으로 만든다. | `compose-rust/src/ui/mod.rs:500`, `compose-rust/src/schema.rs:2421` |
| `src/ui/cr/agents.rs:854`(`HeadedCard`) | `ui::Modifier`에 최소 높이가 없다. Ember는 그 높이의 폭 0 스페이서를 행에 버팀목으로 넣는다. compose-rust#778은 제목으로는 최대 높이 이슈이고, 그 수정이 최소 높이도 주는지는 핀에서 알 수 없다. | `compose-rust/src/ui/mod.rs`(`width`, `height`, `size`뿐) |
| `src/ui/cr/components.rs`(`DataNamedOverlay`) | 클릭 컨테이너에 소유한 접근성 이름이 없다. `Modifier::label`은 서로 다른 이름을 프로그램이 끝날 때까지 모두 쥐고 4096개를 넘으면 버리므로 정해진 이름에만 맞는다. 내용을 가진 `Button`(compose-rust#425)은 자식을 디자인 시스템의 16 dp만큼 안으로 밀고 바꿀 방법이 없어서, 그린 행을 그 안으로 옮길 수 없다. 그래서 데이터로 이름을 짓는 상자는 위에 투명 `Button`을 둔다. | `compose-rust/src/ui/mod.rs`(`keep_label`, `MAX_LABELS`), `renderer/desktop/src/renderer/HostButton.kt`(`labelPadding`) |
| `src/ui/cr/channel_owner.rs:467` | `TextField`에 읽기 전용 설정이 없다. 초대 링크를 선택하고 복사할 수 있게 켜진 필드에 보여 주므로, 입력하면 그려진 글자가 바뀐다. | `compose-rust/src/ui/widgets.rs`(`TextFieldScope`, 1163행) |

### 이번에 바꾸지 않은 메모

`src/ui/project.rs:12`, `src/ui/session.rs:14`, `src/ui/characters.rs:9`는 예전 Dioxus 화면의
빈 곳을 번호 없이 적는다("no clickable Row", "no back stack", "no animation"). compose-rust
화면이 아니라 모델 파일의 설명이다. 맞는 툴킷 이슈는 compose-rust#372(핀에 있음), #302(열림),
애니메이션 API(핀에 있음)다. 이 파일들을 다음에 고칠 때까지 그대로 둔다.
