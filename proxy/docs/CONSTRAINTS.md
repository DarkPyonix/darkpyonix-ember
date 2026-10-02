# CONSTRAINTS.md — read this before touching the code

These are the constraints and traps that cost the most time. Every entry here is a measured
finding against a real VS Code Web build, not a guess. Where an entry says something does not
work, it was tried.

---

## 1. The load-bearing fact about VS Code Web's layout

**VS Code Web lays out against `window.innerWidth`/`innerHeight` only, and computes part
heights from internal constants. It ignores a shrinking container, and its grid re-cancels any
`top`/`margin` offset applied to a `.part.*`.**

Consequently:

- ✅ **What works**: padding on inner flex content. For example, chat
  `.pane-body { padding-bottom }` lifts the input box above the bar.
- ❌ **What does not work**:
  - `.part.editor { top }` → the grid cancels it; the editor does not move.
  - Shrinking `.monaco-workbench` height → ignored; the editor overflows to window height.
  - The terminal's xterm is a **fixed-size canvas** VS Code sized to the window — CSS cannot
    reflow it.
- **Residual overlap**: (1) roughly the bottom two terminal rows sit behind the bottom bar
  (scrollable, minor); (2) the editor tab bar can slightly overlap the top status bar (pixel
  alignment is unstable because of grid cancellation).

### What the cancellation actually is (measured)

On initial load, margin and transform offsets do apply. But **after a layout event (toggling a
panel, maximizing, …) VS Code re-cancels the offsets on the editor and the panel** — transforms
included; it moves the base position back so the net result is zero. Side bar and auxiliary
side bar part transforms do survive, as measured.

Stability per offset:

| Offset | Result |
|---|---|
| statusbar `width` `!important` override (beats the inline width) | **stable** |
| sidebar / aux `translateY` | survives |
| editor tab `margin`, panel `translateY` | **cancelled after a toggle** |

### The fix: the `xmoLiftTopCovered` measurement-based watcher

Every 0.8 s it measures the real coordinates of the part titles (editor tabs, panel/aux/side
bar headers) and applies a `translateY` (`!important`) to the title equal to however much the
top status bar covers. A transform is invisible to VS Code's own layout, and because the value
is measured, it recovers on the next tick even if re-cancelled; when nothing is covered the
lift is 0, so nothing moves unnecessarily.

**Verified portrait and landscape, three toggle cycles each: clear at every step, lift values
pinned at 17/22 with no drift.** Side effect: a lifted tab covers the top 17–22 px of the
content below it (the breadcrumb area) — minor. It runs alongside the static CSS offset (the
editor-tab margin used for initial load).

### Part-level `translateY` has been removed entirely

Not only did it get re-cancelled — it **misaligned the resize sashes (which live outside the
part) from the visual boundary** and broke drag-resizing. Title occlusion is handled solely by
the watcher above.

### Sash touch targets and reachability

1. Horizontal sash `z-index: 1005` (above statusbar's 1004), so the only active sash of a
   maximized panel — the top one, which was buried under the statusbar — can be grabbed. The
   cost is a 4 px click band lost on the statusbar.
2. Vertical sash width 12 px, as a touch target.

**Verified: in portrait, dragging the top sash of a maximized terminal resized it 787 → 472
(which also un-maximizes it); dragging the agent (aux) vertical sash resized it 342 → 170.**

Note: **while a view is maximized, VS Code locks every other sash to disabled.** They all
become active again once it is un-maximized.

---

## 2. The real fix: iframe wrapping

Implemented and verified. The wrapper is `static/frame.html`: the parent page owns the
navigation bar (bottom in portrait, left in landscape) and loads VS Code into an iframe sized
to exclude that bar (`xmo=embed`).

Inside the iframe, embed mode turns the overlay completely off (native layout) and exposes only
the high-level `window.__xmo` API (filesTap / terminal / agent / view / menu / theme) for the
parent bar to call. Chat capture works in embed mode too.

**Verified with Playwright at 390×844 and 844×390**: the workbench lays out at exactly the
iframe size, and the status bar, chat input and terminal all appear fully above the bar —
**the overlap disappears entirely.** The terminal clipping and editor tab overlap are solved at
the root in frame mode.

Frame mode does not move the status bar to the top (there is nothing to occlude, so it stays
natively at the bottom).

⚠️ **CSS trap**: an iframe is a replaced element, so it does not stretch when given `top` plus
`bottom` — it collapses to its intrinsic 300×150. Always use explicit `width`/`height`. There
is a comment about this in `static/frame.html`.

### Parent bar active highlight

The parent polls embed's `__xmo.state()` (files/terminal/agent booleans) every 1.5 s and
re-syncs right after a tap (`syncActiveSoon`). Mutual exclusivity and side-bar occlusion while
maximized are both reflected accurately (verified).

### Bar placement

Redesigned after user feedback ("like vscode.dev"). In embed mode the native activity bar is
made transparent — the DOM stays, so clicks can still be forwarded to it.

- **Landscape**: the iframe takes the full width and the parent bar is **overlaid on the column
  the activity bar vacated** (top = below the titlebar, bottom = above the statusbar, measured
  via `__xmo.metrics()` into `--xf-title-h` / `--xf-status-h`). The titlebar and statusbar use
  the full width, so the impression matches native.
- **Portrait**: a bottom bar, with the iframe slid left by the gutter (`--xf-gutter`, measured
  via `__xmo.gutter()`); the titlebar and statusbar get `padding-left: 48px` to compensate.
- A full-screen portrait side bar overlay was tried and **rolled back on user feedback** (it
  read as a floating layer). The side bar stays native and inline.

Remaining rough edges: the native activity bar (48 px) inside the iframe is still visible
alongside the parent bar, and the Maximize label can disagree when the panel position is set to
the right.

**Frame mode is now chosen by the server from the User-Agent, with no URL parameter.** Desktop
gets the stock screen plus a width-responsive overlay bar.

---

## 3. 🔥 Root causes of the tablet lockup (all fixed — read this)

On a real device (a Galaxy Tab) the whole workspace froze the moment it opened. The cause was
**three latent proxy bugs stacking up**. On the desktop, every one of them happened to be
masked.

1. **The Host header was not preserved.** The relay function stripped Host, so serve-web wrote
   `remoteAuthority: 127.0.0.1:9093` into the HTML, and the browser then tried to open the
   remote WebSocket **directly to 127.0.0.1, bypassing the proxy**. On the development machine
   that address happened to work, so everything looked fine; on the tablet it was a dead
   address → remote connection failure → boot loop → renderer hang.
   **Fix: preserve the client's Host all the way to the upstream** (two places: `proxy.relay`
   and `proxy.fetch`). This is also why **connecting from any host works automatically** — and
   why moving to DDNS, a tunnel or Tailscale requires no code change.

2. **A WebSocket route that died instantly.** `@app.websocket_route` does not inject path
   parameters, so **every WS connection crashed** with `websocket_proxy() missing 'full_path'`.
   Bug 1 meant no WS had ever reached the proxy, so this had gone unnoticed.
   **Fix: use `@app.websocket` instead.** That day was the first time the WS relay actually ran.

3. **Injection scope was too wide.** The middleware injected into every HTML response, so the
   overlay JS reached **VS Code's internal documents too
   (`webWorkerExtensionHostIframe.html`)**, contaminating the extension host — and the service
   worker then cached the contaminated copy, which contaminated even the stock test.
   **Fix: inject only when `path == "/"`; CSS merging also honors `xmo=off` on the referrer;
   added an `xmojs=off` bisection switch.**

Also fixed alongside: **the relay was converted to streaming** (a shared `AsyncClient` +
`aiter_raw` + `BackgroundTask(aclose)`). It used to buffer the entire response, which made the
first load very slow and triggered remote-connection timeouts. 16.6 MB now takes 0.7 s.

### Debugging techniques that paid off

- `&xmodebug=1` on the frame wrapper shows a diagnostic panel (UA, tick, screenOri, vv, iframe,
  statusbar, last 4 errors).
- Wireless adb pairing reaches the tablet's Chrome over CDP:
  `adb pair IP:PORT CODE` → `adb connect` →
  `adb forward tcp:9222 localabstract:chrome_devtools_remote` (needs Android platform-tools).
- Chrome's "Desktop site" mode ignores the viewport meta, which breaks rotation and touch
  entirely. The wrapper detects it and shows a warning banner.

---

## 4. Touch does not produce a click inside VS Code

Measured: tapping the command center delivers only `touchstart`/`touchend` into the iframe —
**no native `click` is generated at all**, because VS Code's own gesture handling swallows it.
That is why title bar controls looked dead on a phone.

- Fix: `xmoInitTouchClickBridge()` — if no click arrives within 350 ms of a `touchend` on a
  title bar control, we synthesize and dispatch one.
- ⚠️ **It must be dispatched on `e.target`, the deepest element.** Measured: dispatching on the
  `.command-center` container does **nothing**; dispatching on the lowest element returned by
  `elementFromPoint` (`agent-status-label`, etc.) **opens it**. Use `closest()` only to decide
  *whether* to bridge.
- Activity bar items, by contrast, work fine when dispatched on the container. It differs per
  control, so measure when adding a new one.

---

## 5. The phone keyboard policy — "it appears only when tapped"

The soft keyboard used to rise on its own and refuse to go away. The original approach was
"dismiss when tapping outside"; on the user's suggestion it became **never raise it in the
first place**.

- ⚠️ **In VS Code 1.132 the editor input surface is not a textarea.** It uses the EditContext
  API — a `div.native-edit-context` (measured: that is `activeElement` right after load). That
  is why code which only blurred textarea/input did nothing.
- **Scope is everything** (per the user's request): editor, terminal, search box, chat — the
  autofocus of every input is suppressed. The one exception is the quick input, because it is
  opened by pressing the command center (not a text surface), and suppressing it would make the
  palette untypeable.
- Rule: remember whether a `pointerdown` landed on a **text surface**
  (`.monaco-editor` / `.interactive-input-part` / `.quick-input-widget` / inputbox / input
  element) in `xmoTappedInput`; when `focusin` fires and that flag is false, blur **only** the
  editor and chat inputs. The quick input and find boxes are never touched, because the user
  opened them deliberately.
- The terminal counts as intent even when the `.xterm` canvas is tapped (the real input is the
  hidden `textarea.xterm-helper-textarea` behind it).
- A tap on the parent bar never reaches iframe focus, so it is forwarded via
  `__xmo.dismissKeyboard()`.
- Kill switch **`&xmokb=off`** — disables all of it immediately if the intent detection gets it
  wrong and blocks typing.

---

## 6. ❌ A path tried and abandoned: `workbench.activityBar.location`

Injecting this setting through `configurationDefaults` in the
`<meta id="vscode-workbench-web-configuration" data-settings="…">` that serve-web plants
**does actually work** (all four values confirmed by measurement). It is still unusable for us:

- `hidden`: the column disappears completely (`.part.activitybar` becomes 0×0 and the side bar
  sits at x=0), but **the icon DOM disappears with it** — and our bar forwards clicks to those
  items and reads their icons and labels.
- `top` / `bottom`: the icons move **inside the side bar part** (`part sidebar … .composite-bar`),
  so they vanish when the side bar is closed.

In other words, no value gives "column removed" and "icons retained" at the same time.
**Do not try this again.**

---

## 7. Orientation must not be decided by a media query

The wrapper makes the iframe 48 px **wider** than the window (a hidden gutter) and one bar
shorter, so a near-square window reads as landscape inside the iframe while the device is in
portrait. Portrait correction CSS then silently stopped applying, and the right end of the
title bar (the layout controls) hung 48 px past the screen edge.

| Window | Device | Inside the iframe | Result |
|---|---|---|---|
| 950×912 | landscape | landscape | fine |
| **910×912 – 812×912** | **portrait** | **landscape** | ❌ portrait CSS not applied |
| ≤ 809×912 | portrait | portrait | fine |

Fix: the parent decides with `matchMedia('(orientation: portrait)')` and passes the initial
value as `xmoorient=portrait|landscape` on the iframe URL, then keeps pushing it through
`__xmo.setChrome(on, portrait)`. Embed sets `:root[data-xmo-orient]`, and **every portrait-only
rule is keyed on that attribute, never on a media query.** The JS side uses the `xmoPortrait()`
helper (`xmoMaximizeSoon` / `xmoSoloPortrait` / `xmoPlaceMenuAt`). Overlay (non-embed) mode
still uses its own media queries.

Fixed at the same time: the title bar guard in `xmoEmbedStatusTop()` only checked
`position`/`top`/`height`/`left`, so when VS Code rewrote `style.width` inline during a layout
pass (which drops our `!important` priority in the CSSOM), the width returned to the full iframe
(958 px) unnoticed. `right` and `width` are now part of the guard.

**Verified at 950/910/880/850/815/812/809/780 × 912**: the right edge of the title and status
bars matches the viewport width exactly (previously +48 px in the 910–812 range).

---

## 8. Other development-environment notes

- A dead `code serve-web` frequently holds its port with an exclusive binding and cannot be
  killed. Move to the next port and tell the proxy with `XMO_UPSTREAM_PORT`.
- Browser automation's `setViewportSize` is sometimes ignored (the page stays pinned at 1107 px
  wide). **Opening a new page** applies 390×844 portrait correctly. Confirm the real
  orientation with `matchMedia('(orientation: portrait)')`.
- During development Copilot was not signed in, so pressing Enter in chat neither sends nor
  clears the input — captures can therefore run together with the previous text. With a real
  signed-in session it clears.
