# BACKGROUND.md — DarkPyonix (mobile VS Code Web)

> The full context, structure, implementation, decisions and next steps, written so another
> developer or agent can pick the work up. The hard-won traps live in their own document:
> **[CONSTRAINTS.md](CONSTRAINTS.md)** — read that one first if you are about to change code.

---

## 1. Goal

- Make **VS Code Web usable on mobile** — tablets and phones.
- Method: **DarkPyonix**, a FastAPI reverse proxy, sits in front of `code serve-web` (the
  upstream VS Code Web), intercepts requests, and **injects a mobile-responsive overlay
  (CSS + JS)** into the HTML and CSS it passes through.
- Eventual goal: ship this mobile conversion as a **VS Code extension**. With the extension
  running, DarkPyonix injects freely; with it off, output passes through untouched. (Only the
  receiving end is built; the gating switch and the extension itself are on hold — see §6.)
- The upstream VS Code server must not know the proxy exists, which is why the WebSocket is
  relayed too (`dpx/vscode/proxy.py`).

---

## 2. Architecture and components

| Component | Location | Role |
|---|---|---|
| **Upstream VS Code Web** | `code serve-web` (127.0.0.1:9092 by default, changed with `XMO_UPSTREAM_PORT`) | The actual VS Code Web |
| **The DarkPyonix proxy** | `main.py` + `dpx/` (:8888) | Relays every request upstream, injects HTML/CSS, relays WS, serves the home and login pages |
| **The overlay** | `static/overlay.css` + `static/overlay.js` | The mobile UI injected into VS Code Web |
| **Home launcher / login / wrapper** | `static/home.html` · `workspace_login.html` · `frame.html` | Workspace-picking home + login gate + iframe wrapper |
| **ember (the Tauri desktop app)** | the `darkpyonix-ember` repository | The desktop shell. **Currently Tauri starter boilerplate, unimplemented.** Under option B, all it needs to do on launch is load a webview at `{server}/` |

- Example deployment point: `my-pc.example.com`, where DarkPyonix runs.
- `_xmo_recent.json` holds the list of recently opened workspace folders (server-side recents).
- For **where** a file lives, the "Files" section of [../README.md](../README.md) is
  authoritative. This document carries *why it is the way it is*.

---

## 3. What is implemented

### Modality: give everyone the wrapper, and let the wrapper turn its chrome on responsively

Having `&xmo=frame` stuck in the address bar was the problem. Now **`/?folder=...` is the whole
thing**: `_wants_frame()` wraps **regardless of device type** unless `xmo` is `on`, `off` or
`embed`. The wrapper (`static/frame.html`) then decides for itself, from the viewport, whether
to show its chrome:

| State | Condition | Result |
|---|---|---|
| **chrome off** | `(pointer: coarse), (max-width: 1366px)` does not match = wide window with a mouse | Bar hidden, iframe at 100%×100%, embed shows **VS Code's native activity bar and bottom status bar as they are** → indistinguishable from stock |
| **chrome on** | Narrow window, or a touch device | Our bar is shown; the iframe is slid left by the gutter (48 px), pushing **the dead activity bar column off-screen**; the status bar moves to the top |

- The switch happens **on resize, with no reload** (verified: the iframe load count stays at 1).
- Wiring: the parent sets `:root[data-xf-chrome=on|off]` and calls `__xmo.setChrome(bool)`;
  embed sets `:root[data-xmo-chrome=on|off]`, and when off, `xmoEmbedChromeRestore()` strips
  every inline style `xmoEmbedStatusTop()` had planted. The initial value rides on the iframe
  URL as `xmochrome=on|off` to prevent a flash.
- **Why everyone gets the wrapper**: the 48 px activity bar column that the VS Code grid
  reserves **cannot be removed with CSS** (see CONSTRAINTS.md §1). The old overlay mode, which
  drew the bar inside the document, left a "dead white strip" exactly there — the strip the user
  pointed out in a screenshot. Only the wrapper, which pushes the column off-screen by sliding
  the iframe, removes it.
- The login success redirect no longer appends `&xmo=frame` (`static/workspace_login.html`).
- Legacy escape hatches: `?xmo=on` gives the in-document overlay bar without the wrapper (**the
  dead 48 px strip comes back** — a known limitation). `?xmo=off` turns everything off.
- Verified: at 1600 px → `chrome=off`, activity bar opacity 1, status bar `position:static` at
  the bottom. **Shrinking to 895 px → `chrome=on`, and the side bar starts at screen x=0 with no
  white strip.** Back to 1600 px → fully restored to the native state.

### Portrait = one thing at a time, full screen

In portrait, pressing a button on the bar gives that thing the whole screen.

- **Terminal / agent**: VS Code's own **Maximize** (panel / auxiliary side bar), as before.
  Verified: terminal at `[0,57,390,735]` with `Restore Panel` present.
- **Side bar views (Explorer, Search, Git, Extensions, …)**: `xmoSoloPortrait()` first closes
  the panel and the auxiliary side bar (one at a time), and CSS stretches `.part.sidebar` to
  the whole area below `--xmo-top-band`.
- ⚠️ **Why CSS was necessary**: VS Code has **no "Maximize Side Bar"**, and the editor group's
  minimum width is **220 px**, so on a 438 px workbench the side bar is **capped at 170 px** —
  and at that point the sash locks to `disabled` too (measured). It is simply not possible
  natively.
- But **stretching the part alone is not enough** — the inner pane/tree still renders at the
  170 px the grid decided. `width: 100% !important` has to be applied to
  `.composite / .content / .pane / .pane-body / .split-view-view / .monaco-list …` as well for
  the content to follow. **Width beats the inline value and is not re-cancelled** (the statusbar
  precedent in CONSTRAINTS.md §1) — what gets re-cancelled is offsets. Verified:
  `contentW 390`, equal to the viewport width.
- `--xmo-top-band` (status bar + title height) and `--xmo-frame-left` (the gutter) are measured
  by `xmoEmbedStatusTop()` and planted on `:root`.

#### 🔴 Webviews do not live inside the part — so portrait full-screen must use neither `position: fixed` nor `z-index`

Reported by the user as "Claude Code is completely blank in portrait", and pinned down to
appearing only under `&xmofs=nozi`.

VS Code Web mounts a webview as `iframe.webview` → **`.webview-overlay-content`
(`position: fixed; z-index: 21`) → `.monaco-workbench`**. It is **not a child of the part**; it
is a top-level workbench overlay that a script positions against the view's rect (measured:
`insideSidebar: false`).

- Floating the part with `position: fixed` → every webview view is **blank**.
- Giving the slot `z-index: 20` → the same symptom (a stacking contest).
- Removing the z-index → the webview is visible, but **the editor slot comes later in DOM order
  and paints over the widened side bar**, scrambling the screen.
- ✅ **The fix: never create the overlap.** Widen the side bar slot and **collapse** the slots
  beside it:
  ```css
  .split-view-view.visible:has(> .part.sidebar) { width: calc(100% - var(--xmo-frame-left)); }
  .split-view-container:has(> .split-view-view.visible > .part.sidebar)
    > .split-view-view:is(:has(.part.editor), :has(.part.auxiliarybar)) { width: 0; }
  ```
  No `z-index` anywhere. Without `.visible`, **even a closed side bar** stretches to full width.
  The editor must be matched as a **descendant** with `:has(.part.editor)` — the grid nests the
  editor and panel in a separate vertical branch, so a direct-child match fails.
- ⚠️ **The price of a collapsed editor**: while the side bar is open the editor is 0 px, so
  opening something there (a file tab, a Claude Code session) **opens it invisibly behind the
  side bar** — which to the user looks like "I pressed it and nothing happened". So
  `xmoAutoHideSidebarOnEditor()` checks the active tab label every 400 ms and closes the side
  bar if the label changed while it was open (the standard phone pattern: list → tap → full
  screen). Even collapsed, **the DOM is alive, so the tab label is readable**. Verified: from
  side bar 390 / editor 0, opening a new editor gives side bar 0 / editor 390 with the active
  tab `Untitled-1`.
- Measured with Claude Code open: side bar slot 390, editor slot **0**, every z-index `auto`,
  webview overlay `[48,92,389,700]` matching the pane. Closing the side bar returns the editor
  slot to 390.
- **Debug switch `&xmofs=off`** turns portrait full-screen (slot expansion +
  `xmoSoloPortrait`) off entirely and returns to VS Code's native layout (side bar 170 px). One
  refresh separates "the view itself is broken" from "our full-screen broke it". Measured:
  portrait 390 → slot 390 / z-index 20; with `&xmofs=off` → slot 170 / z-index auto.
- ⚠️ **Headless Chromium does not render extension webviews at all** — blank even in stock VS
  Code (`xmo=off`) at 1600×900. Webview problems cannot be verified with Playwright; check
  geometry (rect, computed style) there and confirm on a real device in a real browser.

### Portrait = the editor is full screen too / landscape = work area ↔ agent, half and half

- **Portrait, exactly one editor group**: extension actions ("Claude Code open", "Codex
  sidebar", Open to the Side, …) split the editor, which on a phone means two unusable 220 px
  halves. Using the same slot technique as the side bar, **the active group gets 100 % and the
  rest get 0** (`.part.editor .split-view-view:has(> .editor-group-container.active)`).
- **Portrait, solo when an editor opens**: `xmoAutoHideSidebarOnEditor()` was extended from
  closing just the side bar to `xmoSoloPortrait()` (panel and auxiliary side bar too).
  Otherwise chat stays open and the editor opens as a 190 px sliver (measured).
- **Landscape, agent 50/50** (user request: to watch the work being applied alongside): aux slot
  `left:50%; width:50%`, editor slot `left: var(--xmo-ab-w); width: calc(50% - var(--xmo-ab-w))`,
  primary side bar collapsed.
  - ⚠️ **Not possible natively**: because of the 220 px editor group minimum, even a **real**
    sash drag stops at about 41 % on a 748 px phone (measured 187 → 310). And **VS Code ignores
    synthetic sash drags entirely** — it only processes trusted pointer events, the same
    property as its menus.
  - The inner aux panes need `width: 100% !important` for the content to follow, same as the
    side bar.
  - Verified at 748×274: aux 374 (= 50 %), editor 326 (= 50 % − 48 gutter), confirmed by
    screenshot as work area on the left and agent on the right. Portrait full-screen views (360)
    and showing only the active group when split both still hold.

### Editor tabs at the bottom of the group

Only when the mobile chrome is on (`[data-xmo-chrome="on"]`), the editor tab strip attaches to
the **bottom** of the editor group. Desktop (chrome off) keeps it natively on top.

- `.editor-group-container` stacks its children in **normal flow** (`.title` then
  `.editor-container`, whose height VS Code sets to `group height − title height`). So simply
  taking `.title` out of flow with `position:absolute; bottom:0` raises the editor by exactly
  the tab height and leaves exactly that much space below. **No arithmetic on our side, and no
  offset for VS Code to revert** — it keeps working when the group is resized.
- `top: auto !important` matters. The moment VS Code writes an inline `top`, an absolutely
  positioned box with both top and bottom set **stretches instead of sticking to the bottom**.
- `xmoLiftTopCovered` (the watcher that touches the same `.title`) is gated on
  `data-mobile-overlay`, so it does not run in embed mode — no conflict.
- Verified: portrait 390×844 → group 57..792, editor 57..757, tabs 757..792. Landscape 844×390
  → group 57..390, editor 57..355, tabs 355..390. Desktop 1600×900 with a mouse → `chrome=off`
  and tabs stay stock at the top of the group (35..70) with `position:relative`.

### 🔴🔴 Blank extension webviews on a phone = connecting over plain HTTP (the final cause)

The real reason behind "Claude Code does not appear on the phone". **It had nothing to do with
our layout** — which is why all three `xmofs` switches made no difference.

| Connection | `isSecureContext` | `navigator.serviceWorker` | Webviews |
|---|---|---|---|
| `http://127.0.0.1:8888` (laptop) | true | present | **fine** |
| `http://<LAN IP>:8888` (phone) | **false** | **absent** | **all blank** |
| `https://<LAN IP>:8888` | true | present | fine |

VS Code Web webviews relay their resources through a service worker, and a service worker only
exists in a secure context. → **To use this on a real device, the proxy must be served over
HTTPS.** How to obtain HTTPS is still undecided — see §7-1.

- How to tell the symptoms apart: if an extension panel shows **only its title and is otherwise
  empty**, check whether the address bar says `https` before suspecting layout.
- ⚠️ Headless Chromium cannot draw these webviews even on 127.0.0.1 (which is secure) — a
  separate constraint, so webviews cannot be verified with Playwright at all.

### 🔴 The activity bar overflows on short screens (reported from a real phone)

VS Code **moves view containers that do not fit the activity bar height into an "Additional
Views" (⋯) overflow menu and removes them from the DOM**. Measured:

| Device / orientation | Activity bar height | `.composite-bar` items |
|---|---|---|
| Tablet portrait 390×844 | 735 px | all 7 |
| Phone portrait 360×692 | 583 px | all 7 |
| **Phone landscape 748×274** | **217 px** | **1 — just the overflow chevron** |

**The list has to be read out of the overflow menu**: scanning alone never sees the hidden
containers, which is why a newly installed extension never appeared on the bar.
`xmoScanOverflow(source)` presses the chevron, opens the menu, reads the row labels and closes
it with Escape. Throughout, `:root[data-xmo-scan] .context-view { opacity:0 }` keeps it
**invisible to the user** (measured flash = 0).

- ⚠️ **The auxiliary side bar has its own overflow too** — even at 1600×1000 on desktop,
  `auxBar: ["Chat", "Additional Views"]`. **Codex (openai.chatgpt) never registers in the
  activity bar at all and lives in the auxiliary side bar**, so it is only discovered by reading
  that overflow. The aux composite bar is only in the DOM while that part is open, so we retry
  after boot and scan again **right after the agent button opens aux**.
- ⚠️ The aux overflow menu **also lists the primary side bar's containers**. So what was seen
  directly in the DOM must always win (`e.source = src[0]`). Otherwise Claude Code gets cached
  as `aux` and opens in the auxiliary side bar when tapped.
- Verified: phone portrait caches `Claude Code:activity` and `Codex:aux`; on tap, Codex opens in
  the auxiliary side bar at 360 px, and Claude Code and Explorer in the primary side bar at 360 px.

Two reported symptoms came from this:

1. **"In landscape, pressing a primary side bar view opens the secondary side bar"** —
   `xmoActivateView()` had a fallback that pressed `Toggle Secondary Side Bar` *unconditionally*
   when it could not find the item. It now does that **only when the cached source is `aux`**.
2. **"Claude Code is not visible on the phone"** — opening in landscape first left nothing to
   scan, so the bar was empty. The view list is now **saved to
   `localStorage['xmo_views_<folder>']`**, so the buttons survive on short screens.

**How to open an overflowed container**: press the chevron to open the menu, then choose
**with the keyboard** (`ArrowDown` × index+1, then `Enter`).

- ⚠️ **VS Code menus ignore synthetic mouse events** — confirmed by measurement (the menu just
  stays open). Keyboard works.
- `keyCode` cannot be set through `KeyboardEventInit`, so it must be overwritten with
  `Object.defineProperty`. VS Code's `StandardKeyboardEvent` reads exactly that value.
- Menu row text has the shortcut appended ("Source ControlCtrl+Shift+G"), so match by **prefix**.
- Verified: on a phone in landscape, Claude Code, Testing and Explorer all switch correctly into
  the **primary side bar**. Portrait behaves the same (via the direct click path).

### Bar composition (shared by frame and overlay)

- Order: **Home · Files (Explorer) · Agent · Terminal · (every remaining view) · Settings.**
  Explorer is pulled out of the view list and inserted right after Home (`FILES_RE` /
  `XMO_FILES_RE`). The "more" (⋯) menu and the file long-press submenu were **deleted**.
- **The view list is read live from the VS Code DOM, not hardcoded**
  (`.part.activitybar .composite-bar`, plus `.part.auxiliarybar` when open). That is why
  extension views such as Claude Code and Codex appear automatically — with the old hardcoded
  list they did not even show under "more". The cache (`xmoViewCache`) is sticky, so entries do
  not vanish when a part closes. VS Code's own overflow entry ("Additional Views") and
  Chat/Copilot (duplicated by the agent button) are excluded via `XMO_VIEW_SKIP`.
- **Scrolling**: portrait — the bottom bar is `overflow-x:auto` with buttons at `flex: 0 0 20%`,
  so **exactly 5 slots are visible and the rest are swiped horizontally**. Landscape — the left
  bar is `overflow-y:auto` with buttons fixed at 40 px (dividing the full screen height into
  fifths inflates icons to 80–160 px on a tablet, so the 20 % rule is not used there) and
  scrolls vertically.
- **Settings is pinned bottom-left in landscape only**:
  `position: fixed; left:0; bottom: var(--xf-status-h)` plus `padding-bottom: 54px` on the bar.
  `margin-top:auto` does nothing the moment the list overflows — which is exactly when settings
  is most needed — so it has to leave the scroll flow. In portrait it stays at the end of the
  scroll strip. The overlay mode's left bar is handled the same way.
- All borders were removed from the bar and settings buttons (user request).
- **Popup shadow = the VS Code context menu shadow**: pressing a row in our popup opens a native
  menu above it, and the differing depth looked like a rendering bug. `xmoMenuShadow()` measures
  the real value with a probe div carrying the `.context-view monaco-component` classes
  (`0 0 12px rgba(0,0,0,.14)` — ⚠️ **not** `--vscode-widget-shadow`, which comes back
  transparent here), passes it as `theme().menuShadow`, and the parent uses it as
  `--xf-menu-shadow`.
- **Native menu placement**: VS Code opens the manage/account menus anchored to the *hidden
  activity bar icon*, which in portrait is at the far left of the screen, so they appeared in the
  wrong place. The parent passes the trigger button's rect in iframe coordinates via
  `__xmo.menu(codicon, aria, anchor)` and embed's `xmoPlaceMenuAt()` moves it with a transform
  (rewriting coordinates gets re-cancelled, hence the transform — the CONSTRAINTS.md §1 pattern).
- Tab slots are 20 %, but the **highlight pill is a fixed 52×40 px via `::before`** — painting
  the whole slot turned into a smeared 180 px band on a 900 px window. Icons ride above the pill
  with `.xmo-btn > * { position:relative; z-index:1 }`.
- Redraw happens only when the container set changes (`barSig` / `xmoViewSig`) — regenerating
  every tick resets the scroll position while the user is mid-swipe.
- **Icons**, three cases: (1) codicon — the overlay uses the class as-is, but the frame parent
  has no codicon font, so it gets the `@font-face` URL from inside the iframe via
  `__xmo.codiconFont()`, injects it, and prints **the glyph character itself** obtained from
  `getComputedStyle(el,'::before').content`; (2) an extension `background-image`; (3) an
  extension `uri-icon`'s **mask-image** (this is the Claude Code case — `claude-logo.svg`),
  rendered with `background-color: currentColor` plus the mask so it follows the theme color.
- The settings button **reuses the same DOM node**, so the popup anchor survives a redraw.

### ~~The overlay bar (legacy fallback `xmo=on`)~~ — removed

The mode that drew the bar inside the document. Its root limitation was the **undeleteable
48 px activity bar column** on the left; the wrapper replaced it, and it has now been taken out
of the code entirely (see §8). The workbench now always loads **inside the wrapper's iframe
(embed)**.

### Click forwarding and popups (stabilized)

- `xmoActivate({codicon, aria})` — forwards a click to a native activity bar icon, preferring the
  codicon class, falling back to `aria-label`, retrying up to 6 times.
- `xmoPositionPopup` — places a popup relative to its trigger button (above in portrait, to the
  right in landscape) and clamps it to the viewport.
- `xmoLiftContextMenu` — when a native context menu (manage/account) is covered by the bar,
  pushes it up or right until it is fully visible (automatically, via MutationObserver).

### Home launcher and login (option B)

- `/` with no folder → the **home launcher**: a card grid built from the server recents
  (`/__workspaces`) ∪ folders that have agent conversations (`/__agents/*`) ∪ browser
  localStorage bookmarks (`xmo_home_ws`). Each card carries **that workspace's recent
  conversations**, "Open", bookmark deletion, and "Add workspace" (name / server URL / folder).
- A card's "Open" → `/login?name&url&folder`.
- `/login` → a DarkPyonix-styled login (user / password / server URL, auto-filled) →
  `POST /auth/login` → on success, navigate to `{server}/?folder=<folder>`.
- ✅ Authentication is **implemented**. `dpx/auth/` keeps users (PBKDF2) and sessions in SQLite
  and gates on the `dpx_session` session cookie. The old `/__login` stub, which let anything
  through as long as the username was non-empty, has been **deleted**.
- Home (`/`) and `/__workspaces` now require a login too, because they reveal folder paths and
  conversation previews.

### Recent conversation capture

- In a workspace, pressing **Enter** in the chat input (`.interactive-input-part`) reads the
  `.interactive-input-editor .view-line` text and stores the 8 most recent entries in
  `localStorage['xmo_chats_<folder>']`.
- The home cards read that same-origin localStorage value to show "recent conversations".

### Placement and theming

- **The status bar (including connection state) was moved to the top**, in both orientations,
  directly below the title bar.
- Bottom padding on `.pane-body` in portrait, so the chat input is not covered by the bottom bar.
- Safe area insets (`env(safe-area-inset-*)`) and `viewport-fit=cover`.
- The command palette is placed at `top: safe-top + 8` so it clears the camera notch.
- Theme color: dark theme → white top bar; light theme → black (the `theme-color` meta is kept
  in sync).
- Notification toasts are repositioned above the bottom bar and the status bar.

---

## 4. Code map

For **where** a file is, the "Files" section of [../README.md](../README.md) is authoritative.
This lists only **what to look for inside**.

| Looking for | Where |
|---|---|
| The path a single request takes | the `route_request` middleware in `main.py` — 7 commented steps |
| What gets injected where | `dpx/vscode/inject.py` — laid out as one table |
| Upstream relay (Host preservation, streaming, WS) | `dpx/vscode/proxy.py` |
| The mobile UI itself | `static/overlay.css` + `static/overlay.js` |
| The wrapper (parent bar + iframe) | `static/frame.html` |
| Sessions and login | `dpx/auth/` |

### The main functions in `static/overlay.js`

- `xmoActivate`, `xmoClickAria`, `xmoClickLabel` — forwarding clicks to native icons
- `xmoPositionPopup`, `xmoRepositionOpenPopups`, `xmoLiftContextMenu`, `xmoScheduleLift`
- `xmoRestoreMaximized`, `xmoPartOpen`, `xmoMaximizeSoon`
- `xmoSaveChat`, `xmoReadChatInputText`, `xmoInitChatCapture` — chat capture
- `xmoMobileModality` (device decision), `xmoSyncThemeVars` (mirrors theme variables from
  `.monaco-workbench` onto `:root`)
- `xmoScanViews` / `xmoScanOverflow` / `xmoViewIcon` / `xmoActivateView` / `xmoCodiconFontUrl` —
  collecting, activating and icon-ing view containers
- `ensureBottomBar` (bar and popup DOM), `xmoRenderViewButtons`, `apply` (overlay on/off plus
  orientation), `syncThemeColor`
- `forceActivityBarHorizontal` / `clearForcedActivityBar`
- `xmoSoloPortrait`, `xmoAutoHideSidebarOnEditor` — portrait full screen
- `xmoEmbedStatusTop`, `xmoEmbedChromeRestore`, `xmoPlaceMenuAt` — embed only
- `xmoInitTouchClickBridge` — the touch→click bridge

### Embed mode

`XMO_EMBED` (`xmo=embed`, or `self !== top`) makes `apply()` force the overlay off and expose the
`window.__xmo` API: `filesTap` / `terminal` / `agent` / `view` / `menu` / `state` / `theme` /
`gutter` / `metrics` / `views()` / `activateView(label)` / `codiconFont()` / `setChrome(bool)` /
`dismissKeyboard()`. `XMO_CHROME` is initialized from `xmochrome=on|off` on the iframe URL.

### The webview-only script

`static/webview-kb.js` is a **small keyboard policy** that goes only into VS Code webview host
frames. The full workbench overlay must never be injected into those frames — that is what froze
Android (CONSTRAINTS.md §3).

### localStorage keys

- `xmo_home_ws` — the home workspace bookmark array, `[{id,name,url,folder}]`
- `xmo_chats_<folder>` — that workspace's recent conversations, `[{ts,text}]`
- `xmo_last_user` — the last username logged in with (auto-fills the login page)
- `xmo_views_<folder>` — the view containers collected in that workspace (for short screens)
- `xmo_theme` — the home and login page palette (light/sepia/dark)

---

## 5. Deployment model — everyone runs their own instance on their own machine

This is distributed to several people, but **it is not one instance that many people log into.**
There is no per-user file isolation anywhere in the code (§7-3). Using one account from a laptop,
a phone and a tablet at the same time is supported on purpose (sessions are issued independently
per device; three concurrent sessions confirmed by measurement).

---

## 6. Decisions and next steps

### Decided

- **Option B**: a multi-server launcher-style home (home first, then a per-workspace login).
  Home and login are served by **the web side (DarkPyonix)** because of mobile. ember's role is
  the launcher and login only.
- **Self-signed certificates are abandoned** (§7-1).

### Next steps, in suggested priority order

1. ~~iframe wrapping~~ → **done and promoted to the default.** The login success redirect opens
   in frame mode (verified on a real Galaxy Tab: no freeze, rotation, status bar, resizing, and
   the doubled bar removed). In embed mode the native activity bar is made transparent, with the
   DOM kept so the parent bar can forward clicks. The old overlay mode remains reachable as a
   legacy fallback with a manual `?folder=...&xmo=on`.
2. ~~Real authentication and tokens~~ → **done**: `dpx/auth/` (users, sessions, PBKDF2, cookie
   gate). What remains is a mechanism forcing the default password not to be used.
3. **Extension gating** — **on hold by the user's decision.** The design is settled: a companion
   extension sends a 10-second heartbeat to `POST /__ext/ping`, and the proxy falls back to
   passthrough if no heartbeat arrives within 30 seconds. **The receiving end
   (`/__ext/ping`, `/__ext/state`, `extension.active()`) is implemented and dormant in
   `dpx/vscode/extension.py`**; what remains is the middleware gating switch and the companion
   extension itself (tiny: a package.json plus a heartbeat extension.js). `xmo=on/frame/embed`
   will be kept as dev switches that bypass the gate.
4. **ember (Tauri)**: implement loading a webview at `{server}/` on launch.

### Already called "resolved" by the user

- The icon portrait/landscape placement inversion (fixed).
- Git at that size in landscape is fine (no change needed).

---

## 7. ⛔ Open decisions

These were **deliberately left empty**. The code has a slot for each but no default value —
putting in a half-thought-out default means shipping it, and then never being able to take it
back.

### 7-1. How to obtain HTTPS for phones and tablets

**Status: self-signed certificates are abandoned. No replacement has been chosen.** So the
repository has no certificate, and **it cannot be served over HTTPS — which means extension
webviews (Claude Code, Codex) are blank on real devices** (§3). `localhost` on the laptop still
works.

#### Why HTTPS is required (this part does not change)

VS Code Web webviews relay resources through a service worker, and a service worker only exists
in a secure context (HTTPS or localhost). It is a browser rule and cannot be worked around. See
the measurement table in §3.

#### What was thrown away

`certs/dev-*.pem` and `tools/make_cert.py`, which generated them. It worked, but the price was
high:

- The SAN pins **that machine's LAN IP**, so changing routers or machines means reissuing
- Every browser required clicking through an "Advanced → Proceed" warning each time
- The private key ended up in the repository (blocked by `.gitignore`, but it never belonged there)
- The certificate that was actually committed had already drifted from the current IP and was
  **unusable**

#### Candidates (reviewed, none adopted)

| Method | Advantage | Cost |
|---|---|---|
| **Tailscale** (`tailscale serve`) | Real certificates issued and renewed automatically, not tied to an IP, no port forwarding, works at home and away | A Tailscale app and account per device |
| **Cloudflare Tunnel** | A public address, no client-side app | Needs a domain; traffic goes through CF |
| **DDNS + Let's Encrypt (DNS-01)** | Real certificates, own infrastructure | Install certbot/acme.sh, automate 90-day renewal |
| **mkcert + installing a root CA** | Zero external dependencies, self-contained on the LAN | A root CA on every device — **installing a CA on a phone is a real burden** |

The current leaning is **Tailscale**.

#### No code has to change when this moves

The proxy **preserves the client's Host header all the way upstream** (CONSTRAINTS.md §3, item 1),
so whatever hostname arrives, serve-web writes that host into `remoteAuthority`. A `*.ts.net`
address or DDNS both work as-is. **The only thing that changes is how it is started.**

#### So, the work

1. Pick one of the four
2. Rewrite the README's "Serving over HTTPS is mandatory for phones and tablets" section around it
3. Confirm on a real device that extension webviews actually appear — **headless Chromium cannot
   verify this** (it does not draw webviews at all, §3). It must be a real device in a real browser.

### 7-2. Login is a thin shell

The login, session and user management code works, but:

- `init_db()` **seeds no default account.** One is created only on a first run where both
  `DPX_USERNAME` and `DPX_PASSWORD` are given. Without them there are zero accounts and nobody
  can get in (`main.py` prints a warning at startup).
- To be decided later: leave it to env vars, generate a random one on first run, provide a
  settings screen, or go with Tailscale in §7-1 and drop login altogether.

### 7-3. Security holes to review before distributing

Collected, not fixed. Not urgent in the current shape (everyone on their own machine), but they
must be looked at before distributing or exposing this externally.

| Hole | Status |
|---|---|
| **No per-user isolation** | Whoever logs in sees the files and `~/.claude` of the OS account that started the server. An account is an entry pass, not an identity |
| **No folder restriction** | `?folder=C:\` reaches the entire machine |
| `POST /auth/users` | **Any** logged-in user can add or delete accounts (there is no admin role) |
| Login attempt limits | None (wide open to brute force) |
| Session cookie | Has `httponly` and `samesite=lax`, but **no `secure`** |
| CORS | `allow_origins=["*"]` with `allow_credentials=True`, so the server echoes the request origin back (measured: `ACAO=https://evil.example.com`). **Right now `samesite=lax` prevents actual exploitation**, but the moment `samesite` is touched, conversation history becomes readable |

---

## 8. Change history (most recent first)

> ⚠️ **Older entries use older names.** They predate the file split, so `main.py` constant names
> such as `CUSTOM_OVERLAY_CSS`, `OVERLAY_BOOT_JS` and `HOME_PAGE_HTML` still appear. The mapping
> to current names is in the "File split" entry at the top.

### Editor theme switched to the upstream DarkPyonix Light

The installed extension was a local fork with a renamed publisher, name and version, carrying a
"DarkPyonix Sepia" theme that had never existed in the theme repository. Compared against
`DarkPyonix/vscode-darkpyonix-theme`, the repository's Light theme turned out to be
byte-identical to the local Light, already uses the warm cream palette, and has richer syntax
coverage (41 tokenColors against 19). Sepia was dropped and the repository version installed
unmodified.

Install procedure, since this lives outside the repository: copy the theme into
`~/.vscode-server/extensions/DarkPyonix.vscode-darkpyonix-theme-0.0.1/`, register it in
`~/.vscode-server/extensions/extensions.json` (a `.bak` exists), and **restart serve-web**.
`code serve-web` has **no `--extensions-dir` flag** (verified), so copying into that path is the
only option. Pick the theme in VS Code with `Ctrl+K Ctrl+T`.

### File split and portability

`main.py` was a single 3,167-line file holding the HTML, CSS, JS, API and proxy. It was split up.

- **Screens → `static/`**: `CUSTOM_OVERLAY_CSS` → `overlay.css`, `OVERLAY_BOOT_JS` →
  `overlay.js`, `FRAME_PAGE_HTML` → `frame.html`, `LOGIN_PAGE_HTML` → `workspace_login.html`,
  `WEBVIEW_KB_JS` → `webview-kb.js`, `UPSTREAM_DOWN_HTML` → `upstream_down.html`. The string
  values were extracted with `ast`, so not one character of content changed — which is why
  `node --check` simply works now.
- **Code → `dpx/`**: `config` / `assets` / `auth` (gate, api) / `vscode` (proxy, inject,
  extension) / `home` (api, workspaces) / `agents` / `hub`. `main.py` is down to 226 lines
  (assembly plus middleware).
- ⚠️ **What nearly went wrong in the move**: `auth_api.py`, `hub.py` and `connector.py` located
  their data files with `Path(__file__)`. Moved as-is, the database and state files would have
  been dragged inside `dpx/`. They were all changed to resolve against `BASE_DIR` (the repo root)
  in `dpx/config.py`.
- **Personal data removed**: the DDNS address, user home paths and LAN IP were generalized out of
  the source and the documents.
- **`.gitignore` added** — `darkpyonix.db` (password hashes plus session tokens),
  `_xmo_recent.json` (this machine's folder paths), `certs/*.pem` (private keys).
  ⚠️ **Files already tracked are not ignored** — that needs `git rm --cached`, which had not been
  done at the time of writing.
- **Portability**: `requirements.txt` documents why the `websockets==12.0` pin is a lifeline
  (unpinning it removes the `extra_headers` argument and the WS relay dies entirely), `main.py`
  got a Python 3.10+ guard, and the README got a "First run on a new machine" section.
- Verified with a "new machine" simulation — source only, copied into an empty folder — covering
  automatic database creation, account seeding and no crash on empty state. All 16 smoke checks
  on the existing machine passed.

### Theme and frame polish

- **The editor CSS wash and the frame settings-menu theme toggle were removed.** The
  `:root[data-xtheme="sepia"]` wash in `CUSTOM_OVERLAY_CSS` (about 127 lines), the theme toggle
  in `#xf-settings-menu`, and embed's `applyXTheme` (which injected `data-xtheme`) were all
  deleted, because they conflicted with a real color theme. **The home and login pages' theme
  switcher (`.xtheme`: light/sepia/dark) stays** — that is for the brand pages' colors and is a
  separate thing.
- **The home and login theme switcher was added** (before that): a ☀◐☾ switcher in
  `HOME_PAGE_HTML` and `LOGIN_PAGE_HTML`, stored in `localStorage['xmo_theme']` (default sepia),
  with `:root[data-xtheme=...]` palettes.
- ⚠️ **Later change**: the status bar is now **the topmost row, not below the title bar**
  (`[status bar][title bar][workbench]`). Only the vertical ordering in the description below is
  inverted; the mechanism is unchanged. `metrics()` folds both rows into `titleH`, so the parent
  bar's position calculation is unaffected.
- **The frame-mode status bar was moved below the title bar** — `xmoEmbedStatusTop()` in
  `OVERLAY_BOOT_JS` (embed only): top-shift the workbench by the status bar height (plus a resize
  to force VS Code to re-measure), pin the title bar to the very top with `position:fixed` (which
  avoids the grid view's overflow clipping), then pin the status bar below it, also fixed.
  Horizontal alignment is computed by measuring the iframe's offset via `window.frameElement` plus
  the offset of the first item inside the bar (which fixed both double-shifting and clipping at
  the vertical gutter). The height is measured once and cached (to avoid flicker), and re-pinned
  by an rAF MutationObserver plus a 250 ms interval plus a resize burst. `metrics()` folds the top
  status bar into `titleH` so `xf-bar` starts below it. **Note: this involved a lot of fighting
  with the VS Code grid — the diagnostic flags have been removed.**
- **Frame left bar polish** — the landscape vertical bar went from `space-around` to
  `flex-start` plus `gap`, and the settings button to `margin-top:auto` so it is **pinned to the
  bottom** (native VS Code style). The settings icon was replaced with the official codicon
  "gear" SVG at 16×16.

### `xmo` removed from the URL, and the bar redesigned

Three user reports addressed. Details are in §3 under "Modality", "Bar composition" and
"The overlay bar".

1. **`&xmo=frame` was stuck in the address bar and hid the normal desktop screen** → the
   parameter was dropped from the login redirect, and the server decides whether to wrap from the
   User-Agent. **Desktop's width-based responsiveness — narrowing the window brings the overlay
   bar — is kept.** ⚠️ At one point this was wrongly implemented as "turn the mobile UI off
   entirely on desktop" and reverted; the responsiveness has to stay alive.
2. **The bar's color was wrong when the window was shrunk** → caused by sibling elements not
   inheriting the theme variables, which exist only on `.monaco-workbench`. Fixed by mirroring
   onto `:root` with `xmoSyncThemeVars()`.
3. **Bar composition changed, and extension views were missing** → rearranged to
   Home · Agent · Terminal · (all views) · Settings, "more" deleted, the view list collected live
   from the DOM (Claude Code confirmed), 5 slots visible plus scrolling.
4. **A dead 48 px white strip remained to the left of the overlay bar** (user screenshot) → apply
   the wrapper to every client and toggle the chrome responsively. See "Modality" in §3 and the
   abandoned `activityBar.location` path in CONSTRAINTS.md §6.
5. **Settings pinned bottom-left in landscape**, and **the status bar moved to the topmost row**
   (above the title bar). Verified: `status.y=0 < title.y=22` in both orientations; in landscape
   settings is `position:fixed` with coordinates `[0,344]` unchanged at `scrollTop` 0 and at the
   end; in portrait it still scrolls.
6. **Bar borders removed / order (Home · Files · Agent · Terminal · … · Settings) / popup shadow
   matched to the native menu shadow / portrait native menu moved next to the settings button /
   portrait full screen.** Verified: all four border sides `0px`; measured order matches; popup
   `rgba(0,0,0,.14) 0 0 12px` equals the measured `.context-view` value; the menu's right edge at
   x=382 (below the settings button slot, 312–390) where it used to be at the far left of the
   screen; Explorer, Search and Git all at `[0,57,390,735]` with `contentW 390`, and the terminal
   covering the same area via Maximize.
7. **The right 48 px of the title and status bars was cut off on near-square windows
   (910×912 – 812×912)** → orientation decisions unified into the parent (`xmoorient`,
   `data-xmo-orient`, `xmoPortrait()`) plus `width`/`right` added to the title bar guard. See
   CONSTRAINTS.md §7. Verified: at 8 widths from 950 to 780, the right edge equals the viewport
   width everywhere.

Verification (Playwright, with an admin session cookie injected, **and no xmo parameter in the
URL**):

- Desktop 1600 px → `data-xf-chrome=off`, bar hidden, iframe 100 %, native activity bar opacity 1,
  status bar `position:static` at the bottom. **Shrinking to 895 px → `chrome=on`, iframe
  x=−48 / width 943, activity bar at screen x −48..0 (off-screen), side bar starting at x=0 → no
  white strip.** Returning to 1600 px restores completely. Zero iframe reloads.
- Mobile UA 390×844 → **the frame wrapper with no parameters**, 11 buttons × 78 px (= 20 %) → 5
  slots visible, `scrollWidth 858 > 390`. Scrolling to the end reveals Run & Debug, Extensions,
  Testing, **the Claude logo** and Settings (confirmed by screenshot).
- Tapping the Claude Code button → `views()`'s active goes from `Explorer` to `Claude Code`, a
  real switch. The settings popup is also correctly positioned on the scrolled bar.
- Landscape 844×390: the left bar is `overflow-y:auto` with `scrollHeight 540 > clientHeight 333`.
- Color mirroring proven: changing the workbench's `--vscode-activityBar-background` to
  rgb(20,30,90) made the bar background follow.

### Earlier

- **The tablet (Galaxy Tab S7) lockup on a real device was solved** — see CONSTRAINTS.md §3. Host
  preservation, the WS route fix, narrowed injection scope, and the switch to streaming. Frame
  mode, rotation, the status bar and the absence of freezing were all confirmed on the tablet.
  Remaining observation: re-confirm remote connection stability on a first (uncached) visit.
- **Terminal and agent drag-resizing in portrait was restored** (user report): part `translateY`
  removed, horizontal sash z-bumped, vertical sash widened to 12 px (CONSTRAINTS.md §1). Both
  verified by simulating drags.
- **The status bar occlusion in overlay mode was finally solved**: the `xmoLiftTopCovered`
  measurement-based correction watcher was added (CONSTRAINTS.md §1) — editor tabs and maximized
  panel headers, which used to be re-cancelled after a toggle, now stay below the status bar
  (verified in both directions across repeated toggles).
- **The frame-mode parent bar active highlight** was implemented and verified
  (CONSTRAINTS.md §2), and a **`/healthz` route-ordering bug was fixed** (moved ahead of the
  catch-all plus a middleware bypass — it now returns "ok").
- **Extension gating**: built out as far as the receiving plumbing, then put on hold by the user's
  decision (§6, item 3).
- **Who owns login, confirmed**: the ember repository was the Tauri starter as-is (no login code,
  2 commits). The original plan had ember managing login, but option B moved it to web serving.
  The proposed reading was "screen = web (mobile is mandatory), auth logic = server, ember =
  a token-storing auto-login shell". → **The auth logic was subsequently implemented in
  `dpx/auth/`**; the ember side is still untouched.
- **Two status bar issues in overlay mode** (user reports): (1) in landscape the right 48 px of
  the status bar was pushed off-screen (an inline width of the full viewport plus `left:48`) →
  **fully fixed** with `width: calc(100vw - 48px) !important`; (2) the top status bar covered part
  headers (editor tabs, side bar and chat headers) → solved **as of initial load** with
  `translateY` on sidebar/aux/panel plus a margin on the editor tabs, though editor tabs and
  maximized panels get re-cancelled by VS Code after a toggle (CONSTRAINTS.md §1) — the real fix
  is frame mode.
- **iframe wrapping was implemented and verified** (opt-in `xmo=frame`, CONSTRAINTS.md §2): the
  terminal clipping and editor tab overlap are solved at the root in frame mode. Verified with
  Playwright (installed in the venv, with chromium) for portrait and landscape geometry and
  screenshots.
- Development environment: a stale proxy occupying port 8888 (an old instance started with the
  Store Python) was terminated and restarted under the venv's uvicorn. The verification scripts
  used the pattern of waiting for the workbench through `frame_locator` and then measuring
  `getBoundingClientRect`.
- Remaining: confirm on real devices (phone, tablet) → promote frame mode to the default, clean
  up the duplicated native activity bar, parent bar active highlight.
