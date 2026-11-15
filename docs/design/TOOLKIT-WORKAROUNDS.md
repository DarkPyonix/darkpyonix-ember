# TOOLKIT-WORKAROUNDS.md: Ember workarounds for toolkit gaps (#673)

Status: **marker pass done on 2026-11-15, not compiled.** The table is read from the source of
`ember/app/` on branch `feat/673-marker-pass`. Line numbers are that branch's. No row here was
checked on a display.

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

The first grep of this pass found 186 lines that name a toolkit issue. The rest of them are
references to a fix that is already in use (for example `compose-rust#372` beside a container
`on_click`) and tests that guard against a removed marker coming back. They need no action.

### Removed in this pass

| Place | Workaround | Toolkit issue | State | What replaced it |
|---|---|---|---|---|
| `src/ui/cr/session.rs` `PaneTerminal` | The terminal pane opened at its top and did not follow new output. | compose-rust#385 | Closed, in the pin since v0.0.5 (`ScrollState::scroll_to`) | The pane's `ScrollColumn` takes `remember_scroll_state()` and `FollowNewest` moves it, as the transcript does. Test `pane_terminal_follows_its_newest_line_with_the_toolkit_scroll_state`. |
| `src/ui/cr/compat.rs` `SplitPane` | A fixed two-column row stood in for a split pane. It had no caller. | compose-rust#27 | Closed, in the pin (`compose_rust::ui::SplitPane`) | The function is gone. Test `no_stand_in_split_pane_is_left`. |

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
| `src/ui/cr/agents.rs:591`, `agents.rs:619` | `SelectChip` has a fixed 200 width and `MenuPickerPill` an estimated one, because the `fill_max_size` overlay widens them. | compose-rust#431 | In the pin (`Modifier::match_parent_size`, `ui/mod.rs:474`) | Give `FocusOverlay` `match_parent_size`, drop both widths, capture the 에이전트 screen. |
| `src/ui/cr/vcs.rs:290`, `vcs.rs:4998`, `agents.rs:560`, `routines.rs:1372`, `new_channel.rs:130`, `components.rs:1533` | An invisible `Button` over a painted row or pill carries focus, role and name (30 `FocusOverlay` calls). | compose-rust#425, #429 | In the pin (`Button::content`, `Button::accessible_name`, `Modifier::label`) | Move each caller onto `Button::content` or `components::NamedClickable`, then delete the three `FocusOverlay` functions. |
| `src/ui/cr/vcs.rs:425`, `vcs.rs:5001`, `routines.rs:1877` | Pill widths are estimated with `text_px` and fixed. | compose-rust#427, #328 | In the pin since v0.0.7 | Drop `vcs_pill_width` and let the pill take its content's width. |
| `src/ui/cr/vcs.rs:1933`, `home.rs:328` | Panes scroll on their own; tiles take a fixed height. | compose-rust#389 | In the pin (`Row::equal_height`) | Try `equal_height` and capture both. |
| `src/ui/cr/project.rs:424` | The search field is not focused when it opens. | compose-rust#410 | In the pin (`FocusHandle`, `Batch::request_focus`) | Wire the focus and check it by hand. |
| `src/ui/cr/project.rs:1870` `LeaderOption` | A ring with a filled centre stands in for a radio button. | compose-rust#545 | In the pin (`RadioButton` with `ring_color`, `dot_color`, `size`) | Swap it and capture the project settings sheet. |
| `src/ui/files.rs:50`, `files.rs:15` | `FILE_PICKER_AVAILABLE` is false and the send sheet says the file step is not ready. | compose-rust#483, #485 | In the pin (`compose_rust::FileDialog`, `FileDropTarget`) | Open `FileDialog::open_files()`, fill `SendDraft::files`, flip the constant. |
| `src/ui/cr/home.rs:1987` | No day of the usage chart is selectable. | compose-rust#417 | In the pin (`on_pointer` on a container) | Put the Canvas in a Box with `on_pointer` and show the day's value. |
| `src/ui/home.rs:1313`, `src/ui/cr/project.rs:1679`, `routines.rs:1540` | Chips are split into lines by window width or estimated width. | compose-rust#521 | In the pin (`FlowRow`) | Use `FlowRow` and capture at 390 and 1440. |
| `src/terminal_input.rs:145`, `src/ui/editor_workbench.rs:3070` | A paste chord in the editor terminal shows a notice. | compose-rust#546 | In the pin (`compose_rust::clipboard::read_text`) | Read the clipboard on the chord and check it in the editor window. |
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
| `src/ui/cr/channel_owner.rs:559`, `channels.rs:357`, `home.rs:1508`, `launcher.rs:171`, `new_channel.rs:242`, `new_session.rs:5`, `project.rs:1200`, `project.rs:1712` | Text fields are uncontrolled. | compose-rust#374 | In the pin (`TextField::text`) | Control the value and check that the caret does not move while typing. |
| `src/ui/cr/components.rs:1998`, `channels.rs:627`, `project.rs:371` | A field's frame is drawn on a box around a bare field. | compose-rust#376, #375 | In the pin | Move the frame onto the field. |
| `src/ui/cr/components.rs:32`, `theme.rs:13` | The font stack is not wired into `Label` and `Txt`. | compose-rust#374 | In the pin (`Text::font`) | Wire the stack and capture every screen. |
| `src/ui/cr/components.rs:199`, `components.rs:238` | A modal list stands in for a dropdown popup. | compose-rust#383 | In the pin (`Select`, `Dropdown::closed`) | Compare the toolkit popup with the mockup. |
| `src/ui/cr/theme.rs:341`, `components.rs:1849` | Corners are plain arcs. | compose-rust#294 | In the pin (`Modifier::corner_curve`) | Compare the squircle with the mockup. |
| `src/ui/cr/project.rs:577` | The team area's top corners are covered by a second box. | compose-rust#392 | In the pin (`corner_radii`, `clip`) | Use per-corner radii. |
| `src/ui/cr/project.rs:380` | The header's icon button takes a toolkit glyph. | compose-rust#428, #429 | In the pin | Use Ember's SVG with `accessible_name`. |
| `src/ui/cr/session.rs:5170`, `components.rs:1806` | Escape does not close the drawer; a modal passes its close callback by hand. | compose-rust#476 | In the pin (`on_key_down`, `request_focus`) | Focus the drawer when it opens. |
| `src/ui/cr/session.rs:1285` | The terminal rows are not in a `SelectionContainer`. | compose-rust#5 | In the pin | Try a selection over the styled rows. |
| `src/ui/cr/home.rs:438`, `session.rs:3794`, `project.rs:1347`, `files.rs:506`, `characters_screen.rs:200`, `conversation.rs:257` | `EMBER_CR_SCROLL` leaves blocks out for review captures. | compose-rust#385 | In the pin (`ScrollState::scroll_to`) | Move the capture script onto a real scroll. |
| `src/ui/cr/home.rs:2087` | The stripes take a fixed height. | compose-rust#388 | In the pin (`Canvas::view_box`) | Follow the weighted box. |
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

이번 작업의 첫 grep은 툴킷 이슈를 적은 줄 186개를 찾았다. 나머지는 이미 쓰고 있는 수정을
가리키는 참조(예: 컨테이너 `on_click` 옆의 `compose-rust#372`)와, 지운 표시가 돌아오지 않게
막는 테스트다. 할 일이 없다.

### 이번에 지움

| 위치 | 우회 코드 | 툴킷 이슈 | 상태 | 대신 쓰는 것 |
|---|---|---|---|---|
| `src/ui/cr/session.rs` `PaneTerminal` | 터미널 칸이 맨 위에서 열리고 새 출력을 따라가지 않았다. | compose-rust#385 | 닫힘, v0.0.5부터 핀에 있음(`ScrollState::scroll_to`) | 칸의 `ScrollColumn`이 `remember_scroll_state()`를 받고, 대화와 같이 `FollowNewest`가 옮긴다. 테스트 `pane_terminal_follows_its_newest_line_with_the_toolkit_scroll_state`. |
| `src/ui/cr/compat.rs` `SplitPane` | 고정 두 칸 행이 분할 창을 대신했다. 부르는 곳이 없었다. | compose-rust#27 | 닫힘, 핀에 있음(`compose_rust::ui::SplitPane`) | 함수를 지웠다. 테스트 `no_stand_in_split_pane_is_left`. |

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
| `src/ui/cr/agents.rs:591`, `agents.rs:619` | `fill_max_size` 덮개가 폭을 넓혀서 `SelectChip`은 폭 200 고정, `MenuPickerPill`은 어림한 폭을 쓴다. | compose-rust#431 | 핀에 있음(`Modifier::match_parent_size`, `ui/mod.rs:474`) | `FocusOverlay`에 `match_parent_size`를 주고 두 폭을 지운 뒤 에이전트 화면을 캡처한다. |
| `src/ui/cr/vcs.rs:290`, `vcs.rs:4998`, `agents.rs:560`, `routines.rs:1372`, `new_channel.rs:130`, `components.rs:1533` | 그린 행이나 알약 위의 보이지 않는 `Button`이 포커스, 역할, 이름을 맡는다(`FocusOverlay` 호출 30곳). | compose-rust#425, #429 | 핀에 있음(`Button::content`, `Button::accessible_name`, `Modifier::label`) | 부르는 곳을 `Button::content`나 `components::NamedClickable`로 옮기고 `FocusOverlay` 함수 셋을 지운다. |
| `src/ui/cr/vcs.rs:425`, `vcs.rs:5001`, `routines.rs:1877` | 알약 폭을 `text_px`로 어림해 고정한다. | compose-rust#427, #328 | v0.0.7부터 핀에 있음 | `vcs_pill_width`를 지우고 알약이 내용 폭을 갖게 한다. |
| `src/ui/cr/vcs.rs:1933`, `home.rs:328` | 두 칸이 따로 스크롤된다. 타일 높이가 고정이다. | compose-rust#389 | 핀에 있음(`Row::equal_height`) | `equal_height`를 써 보고 둘 다 캡처한다. |
| `src/ui/cr/project.rs:424` | 검색 필드가 열릴 때 포커스를 받지 않는다. | compose-rust#410 | 핀에 있음(`FocusHandle`, `Batch::request_focus`) | 포커스를 연결하고 직접 확인한다. |
| `src/ui/cr/project.rs:1870` `LeaderOption` | 가운데를 채운 고리가 라디오 버튼을 대신한다. | compose-rust#545 | 핀에 있음(`RadioButton`의 `ring_color`, `dot_color`, `size`) | 바꾸고 프로젝트 설정 시트를 캡처한다. |
| `src/ui/files.rs:50`, `files.rs:15` | `FILE_PICKER_AVAILABLE`이 false이고 보내기 시트가 파일 단계는 준비 중이라고 한다. | compose-rust#483, #485 | 핀에 있음(`compose_rust::FileDialog`, `FileDropTarget`) | `FileDialog::open_files()`를 열어 `SendDraft::files`를 채우고 상수를 바꾼다. |
| `src/ui/cr/home.rs:1987` | 사용량 차트에서 날짜를 고를 수 없다. | compose-rust#417 | 핀에 있음(컨테이너의 `on_pointer`) | Canvas를 `on_pointer`가 있는 Box에 넣고 그 날의 값을 보여 준다. |
| `src/ui/home.rs:1313`, `src/ui/cr/project.rs:1679`, `routines.rs:1540` | 칩을 창 폭이나 어림한 폭으로 줄에 나눈다. | compose-rust#521 | 핀에 있음(`FlowRow`) | `FlowRow`를 쓰고 390과 1440에서 캡처한다. |
| `src/terminal_input.rs:145`, `src/ui/editor_workbench.rs:3070` | 에디터 터미널에서 붙여넣기 단축키를 누르면 안내만 뜬다. | compose-rust#546 | 핀에 있음(`compose_rust::clipboard::read_text`) | 단축키에서 클립보드를 읽고 에디터 창에서 확인한다. |
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
| `src/ui/cr/channel_owner.rs:559`, `channels.rs:357`, `home.rs:1508`, `launcher.rs:171`, `new_channel.rs:242`, `new_session.rs:5`, `project.rs:1200`, `project.rs:1712` | 텍스트 필드가 비제어다. | compose-rust#374 | 핀에 있음(`TextField::text`) | 값을 제어하고 입력 중에 캐럿이 움직이지 않는지 확인한다. |
| `src/ui/cr/components.rs:1998`, `channels.rs:627`, `project.rs:371` | 필드 테두리를 맨 필드를 감싼 상자에 그린다. | compose-rust#376, #375 | 핀에 있음 | 테두리를 필드로 옮긴다. |
| `src/ui/cr/components.rs:32`, `theme.rs:13` | 글꼴 목록이 `Label`과 `Txt`에 연결되지 않았다. | compose-rust#374 | 핀에 있음(`Text::font`) | 연결하고 모든 화면을 캡처한다. |
| `src/ui/cr/components.rs:199`, `components.rs:238` | 모달 목록이 드롭다운 팝업을 대신한다. | compose-rust#383 | 핀에 있음(`Select`, `Dropdown::closed`) | 툴킷 팝업을 목업과 비교한다. |
| `src/ui/cr/theme.rs:341`, `components.rs:1849` | 모서리가 단순 원호다. | compose-rust#294 | 핀에 있음(`Modifier::corner_curve`) | 스쿼클을 목업과 비교한다. |
| `src/ui/cr/project.rs:577` | 팀 영역의 위쪽 모서리를 두 번째 상자로 덮는다. | compose-rust#392 | 핀에 있음(`corner_radii`, `clip`) | 모서리별 반지름을 쓴다. |
| `src/ui/cr/project.rs:380` | 헤더의 아이콘 버튼이 툴킷 글리프를 쓴다. | compose-rust#428, #429 | 핀에 있음 | Ember의 SVG와 `accessible_name`을 쓴다. |
| `src/ui/cr/session.rs:5170`, `components.rs:1806` | Escape로 서랍이 닫히지 않는다. 모달은 닫기 콜백을 직접 넘긴다. | compose-rust#476 | 핀에 있음(`on_key_down`, `request_focus`) | 서랍이 열릴 때 포커스를 준다. |
| `src/ui/cr/session.rs:1285` | 터미널 행이 `SelectionContainer` 안에 없다. | compose-rust#5 | 핀에 있음 | 꾸민 행 위에서 선택을 시험한다. |
| `src/ui/cr/home.rs:438`, `session.rs:3794`, `project.rs:1347`, `files.rs:506`, `characters_screen.rs:200`, `conversation.rs:257` | `EMBER_CR_SCROLL`이 검토 캡처용으로 블록을 뺀다. | compose-rust#385 | 핀에 있음(`ScrollState::scroll_to`) | 캡처 스크립트를 실제 스크롤로 옮긴다. |
| `src/ui/cr/home.rs:2087` | 줄무늬 높이가 고정이다. | compose-rust#388 | 핀에 있음(`Canvas::view_box`) | weight를 받은 상자를 따른다. |
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
| `src/ui/cr/channel_owner.rs:467` | `TextField`에 읽기 전용 설정이 없다. 초대 링크를 선택하고 복사할 수 있게 켜진 필드에 보여 주므로, 입력하면 그려진 글자가 바뀐다. | `compose-rust/src/ui/widgets.rs`(`TextFieldScope`, 1163행) |

### 이번에 바꾸지 않은 메모

`src/ui/project.rs:12`, `src/ui/session.rs:14`, `src/ui/characters.rs:9`는 예전 Dioxus 화면의
빈 곳을 번호 없이 적는다("no clickable Row", "no back stack", "no animation"). compose-rust
화면이 아니라 모델 파일의 설명이다. 맞는 툴킷 이슈는 compose-rust#372(핀에 있음), #302(열림),
애니메이션 API(핀에 있음)다. 이 파일들을 다음에 고칠 때까지 그대로 둔다.
