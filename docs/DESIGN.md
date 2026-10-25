# DESIGN.md: the Ember client design

This document records the client design the owner approved on 2026-10-25, before it is built in
`dioxus-compose`. The decisions and the owner's words are in `INTENT.md` D25 to D35; the
requirements are in `SPEC.md` §L (`FR-L10` to `FR-L13`, and `FR-L15` onward). This file holds
the values and layouts those entries point to, so the code has one place to copy them from.

**Visual source.** The interactive mockup in `.scratch/design-mockup/` (ignored, kept on the
leader's machine) is the reference: `index.html` links every screen at four widths (1440, 1024,
768, 390), with a light/dark and an iOS/other preview switch. Where this document and the mockup
disagree, this document wins, because the mockup also carries demo shortcuts. `style.css` there
holds the tokens below; `FILES.md` holds the shared component APIs; `REVIEW.md` holds the visual
review that set the type scale.

**Status.** Design approved, not built (#154). The logo is the one part still waiting for the
owner's final sign-off (see "Logo"). Gaps in `dioxus-compose` and `compose-rust` that block parts
of this design are listed at the end.

Tags follow `INTENT.md`: **[user]** is the owner's own decision, **[provisional]** is the
team's reading or detail that the owner has not confirmed.

---

## 1. Principles

- **One design on every OS.** [user, D25] macOS, Windows, Linux, Android and iOS draw the same
  screens with the same tokens and components. No per-OS widget set. The reason: Ember is one
  product that the user moves between machines, and a session must look the same on all of them.
- **The one exception is iOS Liquid Glass.** [user, D25] On iOS only, the floating bottom bar and
  the phone menu button use Liquid Glass (a translucent surface, background blur and saturation,
  a top edge highlight, the content below showing through). Every other platform draws the same
  bar opaque. The reason: the owner asked for it on iOS by name, and the bar is the only surface
  that floats over content on a phone.
- **Calm, warm and dense like the references.** [user for the references, provisional for the
  wording] Warm grey backgrounds, white cards with a hairline border and an almost invisible
  shadow, regular-weight large numbers, pills for status, outline icons. The base is not the cool
  grey of a JetBrains IDE: the owner's later rounds pointed at the reference images (D25). [user,
  D25, 2026-10-25] Tool windows and the IDE screens (editor, terminal) add a JetBrains-like feel,
  because the owner asked for it on those work surfaces. Which areas and how is [provisional]
  until the owner approves a mockup draft (pending).
- **Visual elements over block lists.** [user, FEEDBACK v6: "자꾸 쉽게 쉽게 가려고 블록 리스트
  형식으로 가는데 그건 너무 별로야."] Dashboards use rings, stacked bars, avatar groups and large
  numbers in tiles of different sizes. A stack of same-sized rows reads as a block list.
- **One data palette.** Every chart, dot and badge takes its colour from one palette. Harness
  colours are palette entries, not vendor brand colours, so a screen with three harnesses stays
  calm.
- **Every screen works like the app.** The mockup screens navigate, open details, send, approve
  and persist. The implementation keeps those behaviours; the demo data does not ship.

### Reference images

All are in `.scratch/design-mockup/refs/`.

| File | What it is | Used for |
| ---- | ---------- | -------- |
| `ref1-dashboard.jpg` | A light dashboard app | Sidebar, nav pills, KPI row, card header, list density, pill size (the token measurements in `NOTES.md` come from it) |
| `ref2-agent-app.jpg` | A desktop agent chat app | Thread list, bubbles, command lines, the large composer |
| `ref3-usage.jpg` | A warm usage card | Large numbers, rings, pill bars, the warm tone |
| `ref4-claude-code-desktop.webp` | Claude Code desktop | The single session view's right panel (environment, context, outputs, changes) |
| `ref5-agent-timeline.png` | Agent communication timeline | The status view's lanes, hand-off lines and playback |
| `ref6-darkpyonix-logo.png` | DarkPyonix's temporary logo | The starting point of the Ember logo |
| `ref7-system-monitor.png` | Mole "Status" system monitor | The computer monitor sheet |
| `ref8-blobatar*.png` | blobatar.dev and its editor | Seeded character silhouettes |
| `ref9-vscode-*.svg/png` | VS Code icons | Logo size and facet treatment (only `ref9-vscode-icon-sessions.svg` is the real folded-ribbon mark, per `LOGO-REVIEW.md`) |
| `ref10-model-picker.png` | A model picker popover | The model picker layout |
| `fb8-single-breadcrumb.png` | Owner's screenshot | What the single view must not show (the project breadcrumb) |

---

## 2. Tokens

Names are the mockup's CSS custom properties; the implementation maps them to theme roles.

### Colour

| Token | Light | Dark | Use |
| ----- | ----- | ---- | --- |
| `bg` | `#f3f2ee` | `#151514` | Window background (warm grey) |
| `surface` | `#ffffff` | `#1f1f1d` | Cards, sheets, the session pane |
| `surface-2` | `#f7f6f2` | `#252523` | Inset panels, list panes, chips |
| `line` | `#ebe9e4` | `#2f2e2b` | 1px borders and dividers |
| `text` | `#1c1b19` | `#f0eee9` | Body text |
| `text-2` | `#56534d` | `#c3bfb7` | Secondary text, nav labels |
| `muted` | `#8f8b83` | `#8b877f` | Captions, timestamps |
| `hover` | `#f0eee9` | `#2a2927` | Hover fill |
| `sel` | `#ecebe6` | `#2f2e2b` | Selected row, selected chip |
| `ink` / `ink-text` | `#1c1b19` / `#ffffff` | `#f0eee9` / `#151514` | Active nav pill, primary button, my chat bubble |
| `track` | `#ecebe6` | `#2f2e2b` | Empty part of rings and bars |

Data palette (each has a `-soft` background pair):

| Token | Light | Dark | Soft light | Soft dark |
| ----- | ----- | ---- | ---------- | --------- |
| `c-blue` | `#3f74d4` | `#6b95e0` | `#e6eefb` | `#1e2a3f` |
| `c-orange` | `#e49b55` | `#e6a76a` | `#fbeedf` | `#3a2c1d` |
| `c-sky` | `#9dbde9` | `#8fb1df` | `#edf3fc` | `#1f2834` |
| `c-grey` | `#b4b2ac` | `#8d8b86` | `#efeee9` | `#2a2927` |
| `c-red` | `#d65a4a` | `#e07264` | `#fbe6e2` | `#3b221e` |
| `c-green` | `#4f9a6c` | `#6fb287` | `#e3f1e8` | `#1d3125` |

- Harnesses: Claude Code `c-orange`, Codex `c-blue`, Gemini `c-sky`. Charts, dots, character
  bodies and badges all use these, so a harness is recognisable by colour everywhere.
- Session status: running `c-blue` (with a spinner), waiting for approval `c-orange`, new result
  `c-sky`, done `c-green`, failed `c-red`, offline a grey ring dot. The same mapping is used on
  every screen, so a colour means one state.
- Unread counts are `c-red` circles; bypass permission mode is `c-red` on `c-red-soft` (§4.3).
- Shadows: card `0 1px 2px / 3%` plus `0 1px 4px / 3%`; large (popover, sheet)
  `0 10px 32px / 10%` (45% black in dark). Cards are separated by their border, not their shadow.
- Theme: a three-way choice, **System / Light / Dark** (icons monitor, sun, moon), default
  System, following the OS live. [user, FEEDBACK v7: "다크모드 버튼은 다크모드 켜기 끄기가 아니라
  시스템/라이트/다크 가 되는게 맞지"]

### Type

Font: Inter, falling back to the system UI font and Apple SD Gothic Neo / Noto Sans KR for
Korean. Code and paths: JetBrains Mono. Numbers in tables and KPIs use tabular figures.

The scale has nine steps and nothing in between, because the visual review counted 27 sizes and
35 size/weight pairs on one screen, and half-pixel steps cannot be told apart.

| Token | Size / line height | Typical use |
| ----- | ------------------ | ----------- |
| `fs-2xs` | 11 / 16 | Small pills, badge numbers, chart axis labels |
| `fs-xs` | 12 / 18 | Captions, timestamps, pill text, meta lines |
| `fs-sm` | 13 / 20 | Chips, secondary rows, KPI labels |
| `fs-md` | 14 / 22 | Body text, buttons |
| `fs-lg` | 15 / 22 | Card titles, row titles |
| `fs-xl` | 18 / 26 | Sheet titles, list pane titles, project names |
| `fs-2xl` | 28 / 34 | KPI numbers |
| `fs-3xl` | 32 / 40 | Page titles, the Home greeting |
| `fs-4xl` | 40 / 44 | Hero numbers (routine time, today's usage) |

Weights: **400** regular, **500** medium, **600** semibold. 700 is only for 11px badge numbers;
800 is not used. Large numbers stay at 400 (KPI) or 600 (hero), as in ref1. Letter spacing is in
em only: `-0.02em` for titles and large numbers, `0` elsewhere. A px value would be inherited by
small text and squeeze it (the review found this in the routine detail).

### Spacing and sizes

| Token | Value |
| ----- | ----- |
| `sp1`..`sp8` | 4, 8, 12, 16, 20, 24, 32 px |
| `pad-card` | 16px 20px (card header and body) |
| `pad-row` | 12px 16px |
| `gap-card` | 16px between cards |
| `gap-block` | 12px between blocks inside a card |
| `h-btn` | 40px buttons, search, icon buttons |
| `h-ctl` | 32px small buttons, chips, segmented controls |
| `h-pill` / `h-pill-sm` | 24px / 20px |
| `side-w` | 234px sidebar (72px rail) |
| `tabbar-h` | 64px floating bar, 10px above the bottom edge plus the safe area |

### Radius

| Token | Value | Use |
| ----- | ----- | --- |
| `corner-2xs` | 5px | Keyboard hints |
| `corner-xs` | 6px | Small tiles |
| `corner-sm` | 8px | Icon tiles, small avatars |
| `corner-md` | 12px | Inset panels, inner member tiles, avatars |
| `corner-lg` | 14px | Cards, panes |
| `corner-xl` | 24px | The large composer, sheets |
| `corner-bubble` / `corner-tail` | 18px / 6px | Chat bubbles (the tail corner is smaller) |
| `pill` | 999px | Buttons, chips, pills, nav items, the search box |

---

## 3. The squircle

[user, FEEDBACK v6: "모서리는 스퀘어클로 하고"; v7: "스퀘어클 각도 지금 가로 세로 길이 따라서
회전각이 달라지는데 그건 애플의 스퀘어클과 맞지 않는 구현이야."]

- Every rounded rectangle (cards, panes, tiles, the composer, sheets, inset panels, approval
  cards, channel icons) uses an Apple-style continuous corner: a **fixed radius** from the table
  above with **smoothing 0.6** (Figma's corner smoothing, three Béziers and one arc per corner).
  The curve starts before the radius (about 1.6r) and blends into the straight edge, which is
  what makes it read as Apple's shape.
- The radius never depends on the element's width, height or aspect ratio. Only when the radius
  exceeds half the short side is the smoothing reduced, as Figma and iOS do. The reason: CSS
  `corner-shape: squircle` scales the radius down on small or long elements, which made corners
  look different per size; the owner rejected that.
- Circles and full pills stay plain round (a squircle of a pill is a pill). Asymmetric bubbles keep
  their per-corner radii.
- Fill, border, shadow and clip follow the same path, so a border never shows a round corner under
  a squircle fill.
- Implementation gap: `compose-rust`'s `ContinuousCornerShape` keeps the radius fixed but draws a
  superellipse inside the radius box, so it is more angular than Apple's at the same radius, and
  the schema has no smoothing field (`Shape` and `CornerEach` carry radii only).
  `Modifier::CornerCurve { smoothing }` (or `Shape::Smooth`) is needed in compose-rust, with the
  Figma path as the renderer's curve (see "Gaps").

---

## 4. Components

### 4.1 Buttons, pills, cards

- **Button**: 40px pill, 14/500, 1px `line` border on `surface`. Primary is `ink` fill. Small is
  32px, 13px. Destructive is `c-red` text with a red-tinted border.
- **Icon button**: 40px or 32px circle with a 1px border.
- **Pill**: 24px (20px small), 12/500 (11/500 small), soft fill with the strong colour as text
  (`blue`, `orange`, `red`, `green`, `sky`, grey). Pills carry status and counts, never actions.
- **Card**: `surface`, 1px `line`, `corner-lg` squircle, the card shadow. The header is at least
  56px, 15/600 with an optional 30px icon tile, separated from the body by a 1px line. Body
  padding is `pad-card`. Selected cards use `sel` fill and a 1px `ink` border, never a 2px black
  border (the review found four different selected styles).
- **Inset panel**: `surface-2`, `corner-md`, 12px 16px. Used one level below a card (a project's
  members, a sheet's subsection).
- **Ring and donut**: rings are 88px (76px large in cards, 40 to 52px mini) with a `track` base.
  The status donut has a gap between segments and round ends, with the total in the middle
  (15/600) and an 11/500 caption.
- **Bars**: full-pill bars 22 to 24px wide with 10px gaps, stacked by harness colour.
- **Switch**: 36 by 20, `ink` when on.
- **Leader badge** [user, FEEDBACK v7: "프로젝트 팀에서 리더(오케스트레이터) 역할의 에이전트는
  아이콘에 표기를 따로 해줘야지"]: a small `ink` circle with a gold crown at the character's top
  left, with a 2px `surface` ring, and "(리더)" in its accessible name. It is drawn by the shared
  character renderer only, so every screen shows the same badge: project box, session pane header,
  side list, kanban, timeline (the leader's lane on top), version control team view, monitor
  process rows. The default project has no leader, so no badge.

### 4.2 Chips (`.pickchip`)

The composer's choices are one kind of chip [user, FEEDBACK v11: "홈에서 하네스 선택기만 주변
테두리가 있는데 디자인 깔끔하게 통일해"]: 32px high, **no border**, `surface-2` fill, 13/500, a
16px icon. Project, harness/model, computer and permission mode chips all use it, in that order.
At 390 only the project chip keeps its text; the others become 32px icon chips, so the row never
truncates.

### 4.3 The model picker

One shared component for every place a model is chosen: the Home composer, the session composer,
new member, project settings ("new member defaults"), the routine form [user, FEEDBACK v10: "하네스
선택했을 때 그 하네스가 지원하는 모델을 선택하는 기능이 없네"; v11: "하네스/모델 선택기 디자인 첨부된
사진처럼 바꿔줘"]. Layout from `ref10-model-picker.png`:

- **Chip**: "harness · model · effort", for example "Claude Code · Opus 5.5 · High". Its tooltip
  names the harness's profile (§4.5).
- **Popover**: a vertical **left rail** (a favourites star, then one colour icon per harness), a
  **search field** at the top right, and a list. Each row is one "model (effort)" combination: the
  name in 600, under it the harness icon and name and the profile name, and on the right a
  shortcut (`Ctrl+1` to `Ctrl+9` for the first nine visible rows) and a favourite star. The
  selected row has the `sel` fill. Effort is not a separate segmented control; each effort is its
  own row.
- A model the chosen profile's plan cannot use is dimmed with the reason in red under it; choosing
  it shows the reason as a toast and does not select it.
- Choosing a row closes the popover. A search query searches every harness. Keyboard: focus starts
  in search; up and down move; Enter chooses; `Ctrl+number` chooses directly; Esc closes and
  returns focus to the chip without closing an enclosing sheet.
- Changing the harness resets the model and effort to that harness's defaults. Harnesses with no
  effort setting (Gemini CLI in the demo) show one row per model.
- At 560 and below the popover is a bottom sheet.

### 4.4 The permission-mode picker

[user, FEEDBACK v11: "모드도 필요해 바이패스 모드인지 자동인지 그런거. 새 채팅에는 없더라고."]
A `.pickchip` next to the model chip, in the Home composer, the session composer, new member, the
routine form and project settings. It lists the modes the chosen harness supports:

| Mode | Korean label | Meaning |
| ---- | ------------ | ------- |
| `ask` | 명령마다 확인 | Every tool call asks (today's default) |
| `edits` | 자동 승인(편집만) | File edits are approved, commands still ask |
| `bypass` | 바이패스(모두 허용) | Everything is approved (`FR-A5`'s approve-all) |
| `plan` | 계획 모드 | Plan only, no changes (harnesses that have one) |

Bypass is drawn in `c-red` on `c-red-soft`, so it is never chosen by accident. When the model
chip changes harness, the mode chip moves to that harness and falls back to its default if the
mode is unsupported. Which modes each harness supports is [provisional] until verified against
each CLI (D31).

### 4.5 Profiles in pickers

A profile is an account bundle plus routing rules, bound to a harness (D30). Pickers show the
profile's name small next to the harness, so the user sees which accounts a session will use. No
picker offers a per-member account.

### 4.6 Sheets and the shared header

All sheets (project settings, new project, routine detail, monitor, version control popup,
integration settings, more menu) share one header: 60px high, padding 0 20px, the title in
`fs-xl`/600 on the left, actions right-aligned, a 32px close button at the far right. The review
found three different header alignments before this rule. Sheets have `corner-xl` squircle tops,
the large shadow and a dimmed backdrop; at 560 and below they become full-width bottom sheets.

### 4.7 Navigation and the floating bottom bar

- **Sidebar** (1280 and up): 234px, the logo and name at the top, nav items as 38px pills
  (active = `ink`), the tools (theme, collapse, settings) at the bottom. No "워크스페이스" group
  label [user, FEEDBACK v10: "굳이 워크스페이스 라는 글자를 표기하고 있을 필요가 없을거 같아."].
  Hovering or focusing the logo shows the collapse control in its place.
- **Rail** (1279 and below): 72px icons; expanding overlays the content instead of pushing it, so
  the content does not reflow on a narrow window.
- **Floating bottom bar** (560 and below) [user, FEEDBACK v7: "모바일 뷰에서 지금 하단 앱바가
  안예쁜데 floating앱바 형태로 바꿔줘. iOS에서는 liquid glass 적용하도록 하고."]: 64px high,
  12px from the side edges and 10px above the bottom plus the safe area, radius 26, a soft shadow.
  The active tab is a `sel` pill inside the bar. Tabs: 홈, 프로젝트, 세션, 채널, 더보기; 더보기
  opens a sheet with 루틴, 버전 관리, 에이전트, 파일. Content that must not hide under the bar
  uses the bar's clearance, not a fixed number. On iOS the bar is Liquid Glass (blur 24, saturate
  1.9, translucent `surface`, top highlight); elsewhere it is opaque.

### 4.8 Characters

Team members are drawn as animated characters, not square avatars [user, FEEDBACK v6: "사각
아바타 말고 Dots 처럼 아기자기하고 귀여운 에이전트 아이콘을 만들어두도록 해. 아이콘이라기 보다는 진짜
움직이는걸로 해줘."]. See §7 for the six styles.

### 4.9 Icons

Heroicons outline 24 (MIT), stroke 1.5, round caps and joins, 18px in rows and 16px in chips
[user, FEEDBACK v6: "아이콘팩은 일단 Heroicons 이걸로 하자."]. Icons Heroicons lacks (git,
pull request, stash, device kinds) are drawn at the same weight and listed as Ember's own.

---

## 5. Motion

- **Press** [user, FEEDBACK v8: "버튼 클릭 효과 너무 좀 부자연스러운데 다른 웹페이지들이나 토스에서
  사용하는 효과로 자연스럽게 구성해줘. 너무 빠르게 눌러져서 누르는게 아니라 통통 튀는거 같아."]: every
  pressable element (buttons, tabs, chips, links, pressable cards) scales to **0.975** over
  **180ms ease-out** while held, stays pressed at least **120ms** even on a quick tap, and returns
  to 1 over **200ms ease-out with no overshoot**. A **tint** darkens the element while pressed
  (black at 7% opacity in light, 10% in dark). There is no spreading ripple: the owner found the
  earlier ripple and spring bouncy. Inputs and drag handles are excluded.
- **Characters** breathe and blink at rest, and move by state: busy when running, waving when
  waiting for approval, slumped when failed, smiling when done. Leader badge and state changes do
  not animate layout.
- **Reduced motion**: when the OS asks for reduced motion, the press keeps only the tint (no
  scale), characters stop moving and show their state by pose, and channel icons stop. Live data
  (monitor values) still updates, without transitions.
- Charts and numbers do not animate on load; values replace in place, so a dashboard is calm.

---

## 6. Screens

The menu has eight entries, in this order: **홈, 프로젝트, 세션, 채널, 루틴, 버전 관리, 에이전트,
파일** (D26). Widths: 1440 and 1280 up (sidebar), 1024 (rail), 768 (rail, single column), 390
(floating bar, list and detail swap with a back button).

### 6.1 홈 (Home)

Home is conversation first, with the dashboard directly under it on the same background, so the
two read as one screen [user, FEEDBACK v6].

- **First view**: a greeting (`fs-3xl`/500), the characters of the team at work, and a large
  squircle composer (`corner-xl`) with attach, the chips **project** (default 기본 프로젝트, recent
  projects next) [user, FEEDBACK v10: "채팅 시작할 때 프로젝트 선택 옵션이 있어야 해. 프로젝트가 뭐냐가
  중요하거든."], **harness/model**, **computer** (the project's assigned computers) and
  **permission mode**, and send. Sending creates a new member (session) in the chosen project and
  opens it. Suggestion chips sit below in a three-column grid. No recent-member chips [user,
  FEEDBACK v11: "새 채팅 하단에 있는 ember-리더 이런 애들은 그냥 빼줘."].
- **Dashboard** ("오늘의 팀"), on a 12-column grid with tiles of different sizes (8:4, 7:5,
  4:4:4, 7:5, 12):
  - a KPI row without cards: today's tokens, today's cost, success rate, median approval wait,
    each with a delta pill (green good, red bad);
  - **cost and token trend**: hatched token bars with a cost line, 7 or 14 days, period cost, cost
    per task, cache hit rate (restored as a graph [user, FEEDBACK v11: "대시보드에서 사용량이 원래
    그래프였던거 같은데 왜 바꾼거야? 되돌려줘."]);
  - **usage by weekday**, bars stacked by harness, with account rings for remaining subscription;
  - member status blocks (running, waiting, new result, done) with large numbers;
  - success rate donut (first try, after retry, failed) with a 7-day spark;
  - approval wait: median, histogram (over 10 minutes in orange), a 24-hour heat strip;
  - computer load: CPU and memory meters per online computer (red over 85%);
  - per-member efficiency: character, tasks done, cost per task, success ring;
  - project ring tiles.
- **Integrations**: each card shows what an outside signal does for the team, not a logo wall:
  status pill, account and last sync, a large number, its main rule ("develop CI failed → routine
  'CI 고치기' → core-구현"), the dashboard metrics it feeds, and connect or manage. GitHub (PRs,
  reviews, CI pass rate, last 14 CI runs) is the large card; Google Calendar (meetings and focus
  time, approval batching in meetings), Slack (approvals handled there); Discord and Linear as
  not connected (dashed card); Gmail as needing re-auth. A funnel shows "CI failures → taken by a
  routine → fixed by a member → PR merged". Which integrations ship first is [provisional].
- Usage detail lives in 에이전트 → 사용량; Home links to it.

### 6.2 프로젝트 (Project)

- **Computers row** at the top: small chips (200px wide, 56px high: Heroicons device icon, status
  dot, OS, running count, project count), horizontal scroll with snap, arrows only on wide
  screens, a fade at the end, and an "add computer" chip. Clicking a computer opens the **computer
  monitor** (§6.9) [user, QUEUED: "프로젝트에 있는 컴퓨터 누르면 컴퓨터의 CPU, GPU, RAM, 디스크 사용량을
  작업 관리자 처럼 띄우는 기능을 추가하도록 하자."].
- **Project boxes** in a 3, 2 or 1 column grid. Each box is clearly two levels [user, FEEDBACK
  v6: "프로젝트 박스가 좀 더 시인성이 높게 계층화 된 블록이어야 할거같아."]: the outer card with a
  header (status donut, project name `fs-xl`/600 on its own full-width line, three large numbers,
  then icon buttons for version control, IDE and settings, and an "세션 열기" button), and an
  inset of member tiles in two columns below. Members are named as team members ("ember-리더",
  "ember-구현", "core-구현") with the task as the subtitle [user, FEEDBACK v6: "세션들 자체가
  팀원이기 때문에 @@@구현, @@@리더 이렇게 팀원 형식으로 해줘야지"]. Up to five members show; the
  rest expand in place ("외 N명", then "접기"), with no stacked-card decoration.
- Clicking the box header opens **project settings** (a sheet), not the session [user, FEEDBACK
  v8: "지금은 프로젝트를 누르면 세션으로 이동하는데 프로젝트별 설정 창이 뜨는게 맞는거같아."]: name,
  repository and default branch, assigned computers, team character style (§7), new member
  defaults (harness/model, permission mode), leader, profile override per harness (§6.7), linked
  channels and routines, danger zone (archive, delete; blocked for 기본 프로젝트). Sessions open
  through "세션 열기" or a member tile.
- **기본 프로젝트** replaces "Unsorted": built in, cannot be deleted, holds Home chats until they
  are moved, and has **no leader** (no crown, the leader section says the default project has no
  leader, the timeline has no orchestrator lane) [user, FEEDBACK v10].

### 6.3 세션 (Session)

The 세션 tab is "being inside a project" (D27): it opens the last project's members, and the
header switches project.

- **Two columns** [user, FEEDBACK v9]: the session side list runs the full height on the left
  (search, then one collapsible section per project, the leader first); the session header
  starts at the top of the right column, and the content is under it. The side list's top aligns
  with the header so the divider is one line. 264px at 1440, 236 at 1024, a rail at 768 (opens as
  an overlay drawer), a drawer at 390. It shows in every view.
- **Header**: project switcher, view switcher (멀티, 싱글, 상태), new member, then at the right
  end: **버전 관리** (opens the version control UI as a large sheet for this project, closing it
  returns to the session) [user, FEEDBACK v8], **웹 페이지 열기** (the member's computer IP with
  detected ports, or a typed port), and **IDE 열기** as a split button with the IDE chooser
  [user, FEEDBACK v7: "IDE를 여는 버튼은 오른쪽 끝에 배치하고 어느 IDE로 열건지를 선택하는 버튼을
  추가하는게 맞아."]. All header controls are 32px.
- **IDE chooser**: exactly three entries (D32): **Ember 에디터** (Ember's own editor core, a native
  window), **VS Code** (the computer's `code serve-web` wrapped in Ember's IDE window, runtime OSE
  by default or the official build), **JetBrains Gateway** (a command to the Gateway installed on
  that computer). The choice is remembered.
- **Multi view**: user-defined **tabs**, each holding its own pane layout, saved per project.
  No pane-count picker [user, FEEDBACK v7: "에이전트 수를 고를 수 있도록 하는 기능은 필요 없을거
  같고 탭을 추가해서 여러 pane을 사용자가 배치할 수 있도록 해줘."]. Panes are dragged by their
  header: dropping in the middle swaps, on an edge (24%) splits that side; up to four per row and
  three rows. Members can be dragged in from the side list. Each pane has its composer, approvals
  and a conversation/terminal toggle; double-click opens single view. At 390 panes swipe with
  snap.
- **Single view**: no project breadcrumb and no "back to project" button, only the member's name
  and status [user, FEEDBACK v8: "싱글 뷰에서 굳이 상단에 어느 프로젝트껀지를 표기할 필요가
  없어보여."]. The conversation follows ref2. A **right panel** as in ref4 holds four collapsible
  cards: environment (computer and IP, repository, branch and worktree, harness, permission mode),
  context (a stacked bar with items in k tokens and %), outputs (PRs, artifacts, web pages),
  changes (+/-, files, create PR). At 1279 and below it overlays.
- **Status view** is a dashboard: six metrics; an **agent communication timeline** as in ref5 (one
  lane per member, the orchestrator lane on top, thinking, tool calls, waiting, failure and retry
  segments, hand-off lines, playback at 1x/2x and restart); kanban (waiting, running, approval,
  review, done; five equal columns at 1280 and up, horizontal scroll below); PRs and CI; the
  sub-agent tree; tokens and computer load per member.

### 6.4 채널 (Channels)

- Screen and list are titled **채널**, never 채팅 [user, FEEDBACK v7: "채널인데 채팅으로
  들어가있어."]. The list has one section per project (`FR-L12`), each channel with a grouped
  profile picture, last message, time and a red unread count.
- Each channel has its own **animated icon** (wave, bars, radar, orbit, ripple, sparkle, in the
  project's colour) [user, FEEDBACK v7: "톡방을 톡방 애니메이티드 아이콘을 따로 배정하도록 해."].
- The room follows a KakaoTalk group chat layout [provisional, from the mockup the owner
  approved]: others on the left with picture, name and bubble; me on the right in an `ink` bubble;
  time and unread-reader count beside the bubble; date dividers; agents take part with their
  character and an "에이전트" tag. KakaoTalk's colours are not used: the room is `surface`, other
  bubbles `surface-2` with a 1px line, the input a pill like every other control.

### 6.5 루틴 (Routines)

- **Top dashboard** [user, FEEDBACK v8: "루틴 상단의 대시보드 표기는 마음에 드는데"]: four KPIs,
  a success-rate ring, success/failure bars per weekday.
- **Below**, the earlier list and detail [user, FEEDBACK v8: "그 아래 루틴 리스트는 그냥 전전꺼가 나은거
  같아."]: on the left, routines in one section per project (name and switch, trigger and schedule,
  harness and last run), filter pills in one horizontally scrolling row; on the right the detail
  (the time large in `fs-4xl`/600, the schedule sentence 15/500 with the cron string in mono 12px
  on the same baseline, next run, success ring, run history tiles that open the session, prompt,
  edit, run now). At 390 the list and detail swap.
- The routine form uses the model picker and the permission-mode picker.

### 6.6 버전 관리 (Version control)

[user, FEEDBACK v7: "프로젝트별로 Git을 볼 수 있도록 지원하되, vscode처럼 부족한 UI 말고 Github
Desktop을 대체할 수 있을 정도의 체계적인 UI가 필요해."]

- **Top bar** of three: repository (project), current branch (find, switch, create), remote
  `origin` with a main action that becomes Pull, Push or Fetch by state, plus all three, ahead and
  behind counts and the last fetch time.
- **Sections** on the left with counts: 변경 사항, 히스토리, 브랜치, 풀 리퀘스트, 스태시, 충돌,
  팀 작업.
  - Changes: folder tree with tri-state checks; the diff with line and hunk staging; the commit
    box (summary, description, amend).
  - History: a graph of all branches (a colour per lane, merge commits filled, HEAD emphasised,
    unpushed commits marked), detail with files, branch from here, revert, cherry-pick.
  - Branches: default (protected), work, remote; ahead/behind bars, the member's character, the
    worktree computer.
  - Pull requests: list (state, CI, review pills) and detail (checks, reviewers, line comments,
    review); merge is enabled only with CI passing and an approval, with the reason under the
    button.
  - Stash; conflicts (banner, files, current and incoming side by side with a preview, merge
    commit enabled when all are resolved); team work (per member: branch, worktree path,
    computer, dirty files, ahead/behind, last commit, PR; the leader first).
- Reached from the menu, from a project box and the project screen (opens on that project), and
  from the session header as a sheet. 1440: sections 200 + two panes; 1024: sections become a 56px
  rail; 768: sections become a pill row and panes stack; 390: in-screen tabs show one pane. Diffs
  scroll horizontally inside their own box.

### 6.7 에이전트 (Agents)

[user, FEEDBACK v10: "에이전트 메뉴를 하나 더 추가해서 거기에 하네스와 모델, 그리고 라우팅 등의 우리가
지원하는 기능들을 설정할 수 있도록 세팅해주자. 홈에 있던 사용량 표시도 같이 넣어주고."]

Sections on the left (pills at 900 and below):

- **하네스**: Claude Code, Codex, Gemini CLI, Antigravity, ACP agents: installed, version,
  connection (re-detect, `FR-A6`), run mode (stream-json, app-server, per-turn, ACP), Antigravity's
  sandbox (off by default) and pinned version, the resident shim (`FR-S8`; not for Antigravity yet,
  so its switch is disabled), the per-session memory limit, and the harness's **default
  profile**.
- **모델**: per harness, the supported models and efforts, the default model and effort, plan
  availability pills, the **default permission mode**, and a list visibility toggle.
- **프로필**: account rings at the top; one card per profile (name, target harness, fixed or
  automatic switching, the ordered account list by drag, the switch threshold, routing policy,
  failure handling, optional model proxy, a preview of which account a new session starts on);
  new and delete (a profile in use as a default cannot be deleted). There is no per-member table.
- **네트워크**: the egress switch (agent API traffic leaves through the current computer, D19),
  per-computer IP and connection count, with a "not yet verified (#130)" pill.
- **사용량**: today's tokens, 7/14-day bars, token kinds (`FR-U7`, "unknown" never estimated), cost
  by harness (donut) and by model (bars), account rings per provider.

### 6.8 파일 (Files)

The screen for file transfer between the user's devices (D24 and `FR-F1`..`FR-F9`, in PR #153).
[user, QUEUED: "파일 메뉴도 UI에 추가해줘. ... airdrop 처럼 쓸 수 있게 하는거야!"]

- KPI row (connected devices, transfers in progress, sent this week, direct connection ratio).
- A **device stage**: one tile per device (kind icon, status dot, path pill: nearby green, direct
  blue, relay grey, latency), a "send to an agent" tile, and a tray of files to send. Drop files on
  a tile or press it to pick. Phones and tablets accept before the transfer starts.
- In progress (bar, %, speed, time left, pause, resume, cancel; an interrupted transfer resumes
  through the relay) and received files side by side; history by date with filters.
- An incoming request card (accept or decline); a share sheet in the shape of the iOS one. The
  encryption and path show as one small lock line.

### 6.9 The computer monitor

A sheet opened from a computer chip, laid out after ref7 in the app palette (D33):

- Shared sheet header: name, OS, online state, close.
- Eight cards: health score, CPU (per-core bars, temperature), GPU (line graph), memory (area
  graph, pressure badge, swap), temperatures, disk (free GB large, usage bar), network (send above
  and receive below a centre line, interface), fans (Auto/Cool/Max). Large numbers with small
  units. 4 columns at 1280 and up, 2 below, 1 as a full-screen sheet at 560 and below.
- A process table (name, PID, CPU bar and value, power, memory), sortable by header. Agent
  processes of members running on that computer are highlighted with their character (leader
  crown included) and open the member's session.
- Updates once a second while visible. Offline: "연결이 끊겼습니다" with the last connection time,
  the last values dimmed, no updates.

---

## 7. Team character styles

[user, FEEDBACK v10: "에이전트 캐릭터는 그냥 팀 셋업 할 때 고를 수 있도록 하자. 각각의 디자인의
이름은 니가 적당히 지어봐."] A project chooses one style in project settings or when it is created;
every member of that project is drawn in it. The names are the leader's (the owner left naming
to us), so they are [provisional].

| Style | Korean | Owner's pick | Description |
| ----- | ------ | ------------ | ----------- |
| `mongle` | 몽글 | 1 | The current default (eyes, mouth, cheeks), with more body shapes by harness and seeded colour variation. Default for new projects. |
| `malang` | 말랑 | 2 (candidate B) | A seeded blob silhouette with a soft, springy body. |
| `crew` | 크루 | 3 | A rounded-square body with short limbs and role props (reviewer tie, tester headphones, designer hat, docs bow tie, research beret). |
| `pocket` | 포켓 | 4 (candidate D) | A simple shape mascot with a prop, floating. |
| `cream` | 크림 | 5 (candidate E) | A cream round body wearing the harness colour. |
| `keyring` | 키링 | 6 (candidate G) | A charm hanging from a ring, swaying. |

Rules for all six:

- **Seeded**: the member's name and harness decide the shape and shade, so a member looks the same
  on every screen and every client (the blobatar approach).
- **Harness by colour family**: Claude orange (hue 20 to 34), Codex blue (218 to 238), Gemini sky
  (196 to 212), with lightness and saturation chosen by the seed and toned down to match the
  palette, slightly darker in dark mode [user, "에이전트들 색상은 주변 디자인이랑 맞게 색감 조금 조정은
  더 필요할거 같아."].
- **State by motion**: run, wait, fail, done, idle (§5).
- **The leader crown** is the same badge on every style (§4.1).

---

## 8. Logo

**Temporary, rework pending (rounder silhouette).** Phoenix-fold B is the temporary logo in use in the app and the mockup; the rework (`SPEC.md` FR-L29) is a Draft and not scheduled. Direction [user, QUEUED-logo: "엠버의 로고 디자인은
DarkPyonix의 임시 로고를 참고하도록 해. ... 데스크탑의 작업 표시줄 아이콘은 vscode 처럼 배경 없이
구현하고, 모바일과 앱 내 아이콘은 배경 넣어서 구현해줘."]; candidate B was chosen [user: "그래 뭐
주 후보인게 젤 좋긴 한거같다."] with three refinements (rounder tail, VS Code-like size, more colour
layers), which the ninth independent review found met (`LOGO-REVIEW.md`).

- **Phoenix-fold (B)**: a faithful vector reading of DarkPyonix's temporary logo (ref6): a flame
  whose tail curls up into the bird's neck in one S-shaped flow, drawn as a few folded facets in
  the manner of the VS Code mark, a vertical gradient from yellow `#FFD15A` through orange
  `#FF8A47` to `#FF6450`, and a pink crescent (`#FF4A6E` to `#F0386A`) at the base.
- **Files** (`.scratch/design-mockup/logo/phoenix-fold/`): `mark.svg` (no background: desktop
  taskbar and dock, like VS Code), `mark-sm.svg` (small sizes), `app-icon.svg` (purple night
  background `#2A1748` to `#3a2160`: mobile and in-app), `app-icon-light.svg`, `favicon.svg`,
  `mono.svg`.
- The mark's share of the canvas and its optical weight match the VS Code icon, so the two sit
  together in a taskbar.
- Open option after sign-off: thin the pink crescent's bottom middle to about 9% to be closer to
  ref6.

---

## 9. Tone of UI text

[user, FEEDBACK v10: "UI의 설명들 보면 지금 `새 대화는 Unsorted 프로젝트의 팀원이 된다` 이렇게
이상하게 말투를 잡아놨는데 설명체로 됩니다 이렇게 끝나게 해줘."]

- All user-facing Korean UI text (descriptions, hints, empty states, toasts, tooltips,
  accessibility labels, placeholders) is in 합니다체: "~됩니다", "~합니다", "~없습니다".
  "~하세요" only on buttons and calls to action. No "~다" plain endings. Agents' replies shown to
  the user in demo data follow the same rule.
- Labels are short nouns (세션 열기, 버전 관리, 지금 실행). A name of a feature is the same on every
  screen.
- Internal documents keep the house style (한다체 for Korean documents), which is unrelated.

---

## 10. Gaps in dioxus-compose and compose-rust

From `.scratch/design-mockup/GAPS.md` (source read on origin/develop, 2026-10-25, not built).
Each needs an issue in the owning repository before the screen that depends on it is built.

| Need | State | What is missing |
| ---- | ----- | --------------- |
| Apple-style squircle | Partial | No smoothing in the schema; `ContinuousCornerShape` is more angular than Apple's and only used by LiquidGlass/Cupertino themes (§3) |
| Hover on any widget | Missing | Pointer enter/exit for general widgets |
| Size animation | Missing | `animate_content_size` / `AnimatedVisibility` for expanding rows |
| Arbitrary icon set | Partial | `IconRole` is a closed set of about 27; vector asset icons are needed for Heroicons |
| Per-text font family | Partial | Needed for Inter and JetBrains Mono on desktop |
| Canvas paths | Partial | Curves, fills and gradients (charts, characters, the logo) |
| Backdrop blur | Partial | `Material` exists; dioxus does not map to it, and blur is theme-bound (iOS Liquid Glass bar) |
| Press scale and tint | Partial | Scale transform exists; no spring, no press-scale option; tint is the theme's state layer |
| Width breakpoints | Partial | Window size classes are 600/840dp; the design uses 1280/900/560 |
| Back stack, list-detail | Missing | System back and list/detail swap at 390 |
