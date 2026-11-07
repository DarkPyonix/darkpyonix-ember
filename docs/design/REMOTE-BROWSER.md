# Remote browser and agent browser use (M6, SPEC §R, INTENT D6)

Status: **partial, design for the rest proposed.** The server side (one Chromium per project,
server-held profile, egress through a computer's node, the view stream, the agent DevTools
relay with takeover) is implemented (#29, #44) and tested in CI where a Chrome is present. No
client renders the browser (`FR-R5`), and the acceptance runs with a real Chrome and a real node
are not verified (#12). This document describes what exists, designs what is missing, and ends
with a phased plan and the decisions the owner has to make. Requirements: `FR-R1` to `FR-R14`,
`NFR-RB1` to `NFR-RB3` in `docs/SPEC.md` (the new ones are Draft and [provisional]).

Code: `ember/server/src/browser/`, `ember/server/src/computers/egress.rs`,
`ember/node/src/egress.rs`. Tests: `ember/server/tests/browser.rs`,
`ember/server/tests/browser_egress.rs`.

## 1. Goals and non-goals

Goals, from D6 and Motivation 3:

- One browser identity per project, whose cookies, logins and history live on the main server
  (`FR-R2`), seen from a chosen computer's network (`FR-R1`).
- The user watches and drives it from every client, phone included (`FR-R5`), with no webview in
  the launcher or conversation screens (E1).
- Agents drive the same browser with off-the-shelf DevTools tools, and the user can take over a
  step (a login, a captcha) and hand it back (`FR-R3`).

Non-goals:

- A general web browser for the user's own browsing. The system browser stays the place for
  that, and the 웹 페이지 menu keeps opening it (§6.5).
- Video and audio playback quality. The stream is a screencast of still frames; audio is not
  carried.
- Patching Chrome, chrome-devtools-mcp or Playwright MCP (E2): they are configured, never
  modified.

## 2. Model

```text
 client (compose-rust, no webview)            ember server (main server)                    computer X
 ┌──────────────────────────────┐   view WS   ┌──────────────────────────────────────┐   /egress   ┌────────────┐
 │ browser pane: frame surface  │◀──JPEG──────│ BrowserInstance (per project)        │   (WS over  │ ember node │
 │ + URL bar, tabs, takeover    │──input JSON▶│  headless Chromium, profile on disk  │   HTTP or   │ SOCKS5 exit│──▶ web, LAN,
 └──────────────────────────────┘             │  --proxy-server=socks5://127.0.0.1:p │   transport)│            │    localhost
                                              │      └▶ EgressListener (computer X) ─┼────────────▶│            │
 agent (Claude Code, Codex, ...) on server    │  CDP relay /cdp/<nonce> ◀────────────┤             └────────────┘
   └ chrome-devtools-mcp / Playwright MCP ───▶│   (takeover gate, agent_active)      │
                                              └──────────────────────────────────────┘
```

- One headless Chromium per project runs on ember server (`--headless=new`, 1280×800 viewport).
  The project, not the session, is the unit, because D6 asks for one browser identity and a
  project's members share its logins. Whether that stays so is decision O3.
- Profile: `<EMBER_DATA_DIR>/browser/<project>/profile` (`FR-R2`). `DELETE …/data` wipes it
  (`FR-R4`).
- Browser binary: `EMBER_CHROME_BIN`, else Google Chrome / Chromium / Chrome for Testing app
  bundles, `google-chrome`/`chromium` on `PATH`, else the newest Playwright-cached Chromium.
  None found: `GET /api/browsers` says `chrome: null` and every start fails with that reason.

## 3. Server-held profile (FR-R2, FR-R4)

What exists:

- Chrome runs with `--user-data-dir=<profile>`, so cookies, local storage, IndexedDB, service
  workers and history persist across restarts and egress switches. An egress switch and a server
  shutdown close Chrome with `Browser.close` first (cookies flushed), then wait 10 s, then kill.
- `--password-store=basic` and `--use-mock-keychain`: Chrome never touches the OS keychain (a
  headless server has none on Linux, and on macOS a keychain prompt would block the start).
  Consequence: anything Chrome stores encrypted is encrypted with a fixed, well-known key, so the
  profile is effectively plain text on disk (§8.4).
- `--disable-sync`, `--disable-background-networking`, `--disable-component-update`: the profile
  never talks to a Google account and Chrome makes no traffic of its own that could leave through
  the wrong egress.

Designed here:

- The profile directory is created with mode 0700 and the `browser/` root with 0700 (`FR-R10`).
- Deleting a project removes its profile and its persisted egress row (`FR-R10`).
- A stale `SingletonLock` after a crash is removed at start when the process it names is gone,
  so a crashed Chrome does not lock the profile until someone deletes the file by hand (§10).
- Password saving is turned off (`--disable-features=PasswordManagerOnboarding` is not enough;
  the preference `credentials_enable_service=false` is written into `Default/Preferences` before
  the first start). Sites keep their own session cookies; Chrome does not keep typed passwords
  next to them. See decision O6.
- Downloads go to `<profile>/../downloads/` through `Browser.setDownloadBehavior`
  (`behavior: allowAndName`), never to the server user's home directory, and are listed in the
  pane so the user can move them with file transfer (`FR-F*`). This is phase P8.

## 4. Egress through a computer (FR-R1)

What exists:

```text
Chrome --proxy-server=socks5://127.0.0.1:<port>
  └─TCP─▶ ember server: loopback listener for computer X (ember/server/src/computers/egress.rs)
            └─ one WebSocket per TCP connection: GET <node>/egress (Bearer <node token>)
                 └─▶ ember node X: SOCKS5 server (ember/node/src/egress.rs) ──TCP─▶ target
```

- Egress is chosen per project: `{"kind":"direct"}`, `{"kind":"proxy","url":…}` (`socks5://`,
  `socks4://`, `http://`, `https://`; no credentials) or `{"kind":"computer","id":…}` (a
  registered computer; `local` = direct). The choice is persisted in `ember.db`
  (`browser_egress(project, kind, value, updated_at)`, store migration 5) and reapplied at the next
  start. A computer some project egresses through cannot be removed (409).
- The server listener does not parse SOCKS5; it copies bytes, so Chrome's SOCKS5 client and the
  node's SOCKS5 server talk end to end, and **domain names are resolved on the node** (Chrome sends
  hostnames to a SOCKS5 proxy). Loopback is proxied too (`--proxy-bypass-list=<-loopback>`), so
  `localhost` in the browser is the egress computer's `localhost`.
- The node's `/egress` WebSocket runs on whatever stream the server's `NodeClient` has to that
  node: plain HTTP for a computer registered by URL, a transport stream (`FR-N5`) for a peer
  computer (`NodeClient::over_transport`). Each proxied TCP connection is one WebSocket, so each
  new connection pays one handshake to the node.
- `/egress` format: raw SOCKS5 (RFC 1928) bytes in Binary frames, "no authentication" inside (the
  upgrade carried the node token), `CONNECT` only, IPv4, IPv6 and domain addresses. **An empty
  Binary frame is a half-close** in that direction; Close ends both.
- Node policy: default allow everything (the computer's `localhost` and LAN must be reachable).
  `EMBER_NODE_EGRESS_DENY=private,link-local,loopback,<CIDR>,…` denies destinations (every resolved
  address is checked); `EMBER_NODE_EGRESS=off` refuses `/egress` with 403. Optional plain SOCKS5 on
  the node: `EMBER_NODE_SOCKS_LISTEN=<addr>`.
- A computer that cannot be resolved (removed, node URL invalid) makes the start fail; it never
  falls back to direct.

Designed here:

- **"The session's computer" as the default.** Today the egress is whatever was last chosen for
  the project, and a new project's browser egresses direct. The pane opened from a session (§6)
  opens the project browser with `computer = <that session's current computer>` when the project
  has no persisted choice yet, and shows the egress as a chip that can be changed. Once chosen, the
  egress stays fixed: it does **not** follow later session moves or D20 fallbacks, because a site
  that sees the IP change mid-login may lock the account, and the user chose that vantage point on
  purpose. Decision O2 asks whether the owner wants "follow the session" instead.
- **Egress loss.** When the egress computer goes offline, new connections fail (the node is
  unreachable) and pages show Chrome's proxy error. The server notices through the computer
  registry's online state and sets `ViewState.error = "egress computer offline"` and
  `egress_online = false`, so the pane shows a banner with two buttons: wait, or switch egress. It
  never switches to direct by itself (`FR-R1a`).
- **Egress verification.** `GET /api/browsers/{project}/egress` gains `observed_ip`: the public
  address seen by an echo service through the current egress, fetched on demand by the server
  through the same SOCKS listener (not by the page). The endpoint used is configurable
  (`EMBER_BROWSER_IP_ECHO`, default off, so Ember makes no third-party call unless asked). This is
  what the `FR-R1` acceptance run checks; it is optional in the product (decision O9).

## 5. View stream (WebSocket `GET /api/browsers/{project}/view`)

What exists. Connecting starts the browser if needed and registers a viewer; the screencast runs
only while at least one viewer is connected. All viewers share one stream (one tab, one quality
setting).

### 5.1 Server to client

> There is no `v` field (INTENT D15, #85): fields are only added, unknown fields are ignored, and
> there is no version negotiation.

- **Text** `{"type":"hello","project":…,"state":ViewState}`: first message.
- **Text** `{"type":"state","project":…,"state":ViewState}`: on every state change.
- **Text** `{"type":"error","message":…}`: an input message failed.
- **Binary** frame: `[u32 big-endian header length N][N bytes UTF-8 JSON header][JPEG bytes]`.
  Header: `{"type":"frame","seq","format":"jpeg","target_id","metadata","agent_active","takeover"}`;
  `metadata` is CDP's `ScreencastFrameMetadata` (`deviceWidth`, `deviceHeight`, `pageScaleFactor`,
  `offsetTop`, `scrollOffsetX/Y`, `timestamp`). Frames may be dropped for a slow viewer; each frame
  is a full image, so the client just shows the latest.

`ViewState = {running, egress, egress_computer, target_id, url, title, tabs: [{target_id,url,title}],
agent_active, agent_connections, takeover, viewers, error}`. Fields added by this design:
`egress_online`, `agent_waiting` (an agent command is held by takeover), `takeover_by` (the viewer
id that took over), `downloads`.

### 5.2 Client to server (text JSON; same shapes for `POST …/input`)

Coordinates are CSS pixels in the viewport, the `deviceWidth`×`deviceHeight` space of the frame
metadata; the JPEG may be downscaled (`max_width/height`), so clients scale by
`deviceWidth / jpegWidth`.

| `type` | Fields | CDP |
| --- | --- | --- |
| `mouse` | `event` (`mousePressed`/`mouseReleased`/`mouseMoved`), `x`, `y`, `button` (`left`/`middle`/`right`/`none`), `click_count`, `modifiers` | `Input.dispatchMouseEvent` |
| `wheel` | `x`, `y`, `delta_x`, `delta_y`, `modifiers` | `Input.dispatchMouseEvent` (`mouseWheel`) |
| `key` | `event` (`keyDown`/`keyUp`/`rawKeyDown`/`char`), `key`, `code`, `text?`, `key_code?` (Windows VK), `modifiers` | `Input.dispatchKeyEvent` |
| `text` | `text` | `Input.insertText` (IME, mobile keyboards) |
| `navigate` | `url` | `Page.navigate` |
| `reload` / `back` / `forward` | (none) | `Page.reload` / `history.back()` / `history.forward()` |
| `takeover` | `on` | user takes control: agent commands are held until `on: false` |
| `select_tab` | `target_id` or `null` (follow the newest tab) | re-attach the stream |
| `screencast` | `quality?`, `max_width?`, `max_height?` | restarts `Page.startScreencast` |

`modifiers` is CDP's bit field: Alt 1, Ctrl 2, Meta 4, Shift 8.

Added by this design (§7, P5):

| `type` | Fields | CDP |
| --- | --- | --- |
| `touch` | `event` (`touchStart`/`touchMove`/`touchEnd`/`touchCancel`), `points: [{x, y, id}]`, `modifiers` | `Input.dispatchTouchEvent` |
| `new_tab` / `close_tab` | `url?` / `target_id` | `Target.createTarget` / `Target.closeTarget` |
| `paste` | `text` | `Input.insertText` (clipboard from the client) |

## 6. Viewing without a webview (E1, FR-R5)

### 6.1 The constraint

E1: the launcher and conversation screens never contain a webview. The 세션 screen, where the
browser is opened from, is a conversation screen built on compose-rust. So the browser cannot be a
webview there, and a webview cannot be the shortcut anywhere else either: the page runs on the
server, so a local webview would only show a picture of it anyway, or would run the page locally
and lose the server-held profile (`FR-R2`) and the egress (`FR-R1`).

The remaining shape is **pixel streaming into a native surface**: the server renders, the client
paints frames and forwards input. That is also the only shape that works the same on desktop,
phone and web.

### 6.2 What compose-rust gives today (checked against compose-rust develop, 2026-11-07)

- `Image(asset_id)` draws a registered asset; assets are registered at run time with
  `register_asset(asset_id, AssetKind::Jpeg | Png, bytes)` and dropped with `release_asset`. The
  renderers decode `Jpeg` (`AssetCache.kt`, `decodeRasterAsset`).
- `on_pointer(PointerKinds, handler)` on containers delivers `Move`, `Enter`, `Leave`, `Down`,
  `Up`, `Cancel`, `Wheel` and `ContextMenu` with `x`, `y`, `buttons`, `button`, `modifiers`,
  `click_count`, wheel deltas, `pointer_type` and `pointer_id`. That is enough for mouse, wheel
  and multi-touch, and `consume()` keeps the renderer from scrolling the parent.
- `on_key_down(handler)` delivers `KeyEvent { key, shift, ctrl, alt, meta }`: key down only, a
  logical key, no physical code and no text.
- There is no dedicated frame surface (a node whose pixels are replaced without recomposing) and
  no IME composition events outside a `TextField`. dioxus-compose has none either: PROJECT.md's
  "frame-surface widget" was never built.

### 6.3 Viewer v0: JPEG frames as swapped image assets

The pane keeps two asset ids and alternates: frame *n* is registered under id A, the `Image` node
points at A, then frame *n+1* is registered under B, the node switches to B and A is released.
Only one `Image` attribute changes per frame, so recomposition is one node. The view WebSocket
runs on a background task; it keeps only the newest undrawn frame (older ones are dropped, as the
server already does), and the UI thread picks it up on the next frame tick.

Cost, to be measured (`NFR-RB1`, `NFR-RB2`): each frame crosses the Host/Renderer seam as bytes
(about 60 to 120 KB at 1280×800, quality 70) and is decoded on the renderer side. At 15 frames per
second that is 1 to 2 MB/s across the seam and 15 decodes per second, which should be fine on a
desktop and must be checked on a phone. The asset cache must not keep released frames: a
memory-gate test (the compose-rust pattern of #256) drives 1000 frames and checks RSS stays flat.

### 6.4 Viewer v1: a compose-rust frame surface (upstream)

If v0 misses `NFR-RB1` or `NFR-RB2`, the fix belongs in compose-rust, not in Ember: a
`FrameSurface` widget whose content is a texture the Host updates through a side channel
(`update_frame(surface_id, AssetKind::Jpeg, bytes)` or decoded RGBA), outside the mutation batch,
so a frame never triggers recomposition and the renderer can decode off the UI thread. It is
requested as a compose-rust issue (phase P9) with v0's measured numbers. Ember's pane code only
swaps the widget; the protocol does not change. A video codec (H.264 or VP8 over the same
WebSocket) is a further step that only pays off when bandwidth, not decode, is the limit; it is
not planned (decision O7).

### 6.5 Where the browser appears

The mockup (`.scratch/mockup-v8/session.js`, `webMenu`) has a **웹 페이지** dropdown in the 세션
header: the session computer's detected ports (`FR-P7`), a typed port, and the note "같은 네트워크가
아니면 Ember 터널로 엽니다". Today the app opens these in the system browser (`ide::open_external`),
and a loopback port of a remote computer cannot be opened ("loopback ports open only on the local
computer").

The remote browser is exactly the "Ember tunnel" that note promises: through the computer's egress,
`http://127.0.0.1:<port>` reaches that computer's loopback. So this design proposes:

- A **브라우저 pane type** in the 세션 screen: a pane in the multi view (beside conversation
  panes, using the existing pane tabs), and a tab of the right panel in the single view. On a phone
  it is one full-screen pane like any other. Decision O1 asks whether a separate native window is
  wanted as well.
- The 웹 페이지 menu gets a second action per row, **Ember 브라우저에서 열기**, which opens the
  pane, sets the egress to the session's computer if none is set (§4), and navigates. For a
  loopback port on a remote computer it is the only action offered; for LAN ports both are
  offered, with the system browser first because it is faster. Decision O8.

### 6.6 The pane

Top to bottom: a toolbar (back, forward, reload, the URL field, the egress chip with an online
dot, the tab list as a dropdown, a takeover button), the frame surface letterboxed to the
viewport's aspect ratio, and a status line (agent active, viewers, errors). The URL field is a
compose-rust `TextField`; Enter sends `navigate`. While an agent is active the frame gets a
coloured outline and the toolbar shows the agent's session name (`FR-R3`, "agent is active"
indicator).

## 7. Input forwarding

- **Mouse and wheel.** `on_pointer` on the frame surface, consumed. Pointer coordinates are in the
  pane's dp; the client maps them through the letterbox to the frame's pixels and then to CSS
  pixels with `deviceWidth / jpegWidth` (§5.2). Moves are coalesced to one per frame tick.
- **Touch.** A touch pointer (`pointer_type` touch) is sent as `touch` (`Input.dispatchTouchEvent`)
  so pages see touch events and pinch works; a long press sends `ContextMenu` as a right click. Two
  finger drag scrolls (sent as `wheel`), because a single finger drag must stay a touch drag for
  page widgets.
- **Keys.** compose-rust gives key down with a logical key only. The client sends, per key down,
  `rawKeyDown` then `keyUp` with `key` and the derived `code` and Windows VK for non-text keys
  (arrows, Enter, Tab, Backspace, Escape, function keys, shortcuts with Ctrl/Meta). Printable
  characters are not sent as keys.
- **Text and IME.** Printable text goes through a hidden-looking compose-rust `TextField` that holds
  focus while the pane is focused: the field's committed text is sent as `text`
  (`Input.insertText`) and the field is cleared. This is how IME (Korean, Japanese) and phone
  keyboards work without composition events on a plain node. The composition itself is shown in the
  field, not in the page; that is the visible cost. Key up, physical codes and composition events on
  any node are requested from compose-rust (P9) and would remove the field.
- **Clipboard.** Paste reads the client clipboard and sends `paste`. Copy from the page is not
  carried in v1 (it needs `Runtime.evaluate` of the selection; listed as a later item).
- **Shortcuts the client keeps.** Shortcuts that belong to the app (switching tabs of the app,
  closing the pane) are not forwarded; everything else with focus in the pane is.
- **Several viewers.** Input from every viewer is applied in arrival order; the pane shows the
  viewer count. Takeover (§8.3) is the only lock.

## 8. Agent browser use (FR-R3)

### 8.1 What exists

Agents run on ember server next to the browser and get an unmodified DevTools MCP server pointed
at the relay `ws://127.0.0.1:<port>/api/browsers/<project>/cdp/<nonce>`:

- chrome-devtools-mcp (default): `npx -y chrome-devtools-mcp@latest --wsEndpoint=<relay>`
- Playwright MCP: `npx -y @playwright/mcp@latest --cdp-endpoint=<relay>`

Both attach to the existing browser instead of launching one. Configuration only, no patching (E2):

- **Claude Code**: `--mcp-config=<{"mcpServers":{"ember-browser":{"type":"stdio","command","args"}}}>`
  (equals form, because `--mcp-config <configs...>` is variadic in claude 2.1.288), plus
  `MCP_TIMEOUT=90000` unless set (the first `npx` run downloads the package).
- **Codex**: `codex app-server -c mcp_servers.ember-browser.command="…" -c
  mcp_servers.ember-browser.args=[…] -c mcp_servers.ember-browser.startup_timeout_sec=90`.

Opt-in per server: `EMBER_BROWSER_MCP=chrome-devtools|playwright|off` (default **off**), run by
`npx -y` or `bunx` (`EMBER_BROWSER_MCP_RUNNER`). When on, every agent start gets the server
(`StartRequest::mcp_servers`) plus `EMBER_BROWSER_CDP_WS` and `EMBER_BROWSER_MCP_CONFIG`.

The relay opens one upstream DevTools connection per agent connection and passes messages
unchanged. Each agent command sets `agent_active` (true for 3 s after the last one) and, while the
user has taken over, waits until takeover ends. An egress switch restarts Chrome and closes relay
connections; MCP servers reconnect on their next tool call. The `<nonce>` path is the only route
exempt from the D37 token, because these MCP servers cannot send a header (`docs/SPEC.md`
`FR-S9`).

### 8.2 Designed here: who may drive

- **Per-project grant.** `EMBER_BROWSER_MCP` stays the server-wide master switch. Under it, a
  project setting `browser.agents = off | on` (default off) decides whether that project's sessions
  get the MCP server; it is changed from the pane's menu and from project settings (`FR-R8`).
  Decision O4.
- **Identity on the relay.** The nonce is per server run, so the relay cannot tell sessions apart.
  The agent config instead carries a per-session relay path `/cdp/<session nonce>`: a random value
  minted when the session starts, kept in memory, mapped to the session id, revoked when the
  session ends. `ViewState.agent_sessions` lists who is connected, and the pane names the driving
  session (`FR-R8`).
- **Pinned packages.** `@latest` downloads whatever is published at the moment an agent starts,
  and that code runs as the server user with the browser's cookies in reach. The packages are
  pinned to exact versions in code and updated by PR (`FR-R8`). Decision O9 covers whether to
  vendor them instead.

### 8.3 Designed here: takeover

Today takeover holds agent commands indefinitely, so an agent's tool call hangs until the MCP
client's own timeout, and the agent learns nothing about why.

- `takeover {on:true}` records `takeover_by` (the viewer). Agent commands that arrive are held and
  `agent_waiting` becomes true, so the pane says "에이전트가 기다리는 중".
- A held command waits at most `EMBER_BROWSER_TAKEOVER_HOLD` (default 10 min). After that the relay
  answers it with a CDP error `{"code":-32000,"message":"the user has control of the browser"}`,
  which both MCP servers surface to the agent as a tool error, so the agent can say it is waiting
  instead of hanging (`FR-R9`).
- Ending takeover releases held commands in order. If the session that was driving is known
  (§8.2), the server also delivers a system notice to it: "사용자가 브라우저 조작을 마쳤습니다" (the
  same notice path as the D20 computer notices), so an agent that gave up can continue. This is the
  "agent continues afterwards" of `FR-R3`.
- An agent can ask for takeover: a small MCP tool is not added (E2 keeps the off-the-shelf servers
  unchanged); the agent asks in the conversation, and the conversation already reaches the user.

### 8.4 Credentials and agents

An agent that drives the browser can read what the browser holds: `Network.getAllCookies`,
`Storage.getCookies`, and `Runtime.evaluate` of `document.cookie` or of a password field after the
user typed it during takeover. A page the agent visits can also carry instructions aimed at the
agent (prompt injection). The design does not pretend to close this; it narrows it:

- The relay drops a short deny list of CDP methods that export credentials wholesale
  (`Network.getAllCookies`, `Network.getCookies`, `Storage.getCookies`, `Browser.grantPermissions`,
  `Target.createBrowserContext` with a `proxyServer`, which would bypass the egress) and answers
  them with a CDP error (`FR-R8`). Playwright's `context.cookies()` stops working. Whether either
  MCP server calls one of these during normal navigation is checked against their pinned versions
  in P6 before the list is fixed.
  Decision O5.
- During takeover the agent's commands are held, so it cannot screenshot or read the page while
  the user types a password. After takeover the residual risk stays: `Runtime.evaluate` can still
  read non-HttpOnly cookies and form values. This is documented in the pane's grant dialog, and
  the grant is per project for that reason.

## 9. Security

| Surface | Threat | Control | Status |
| --- | --- | --- | --- |
| `/api/browsers/*` HTTP and the view WebSocket | Any local process or web page drives the browser, reads frames (which show logged-in pages) or wipes the profile | D37: local token (or admitted device key), Origin and Host check on every route, including the WebSocket upgrade | Middleware written (#338); tests for the browser routes added in P1 (`FR-R7`) |
| Agent relay `/cdp/<nonce>` | A local process without the token drives the browser | Unguessable nonce in the path, constant-time compare, never in a query string, per session (§8.2), Origin and Host check still applied | Per-run nonce built; per-session nonce P6 |
| Chrome's own DevTools port | Unauthenticated CDP on `127.0.0.1:<random>` gives full control to any local process | Replace `--remote-debugging-port=0` with `--remote-debugging-pipe` so there is no port at all. The pipe is one connection, so the relay then multiplexes agent connections over it (flattened `Target.attachToTarget` sessions, ids rewritten per agent) instead of opening one upstream per agent | Designed (P1b, the largest server step); today the port exists and `BrowserInfo.debug_port` exposes it |
| Egress listener | Any local process uses a computer's egress (exits from that computer's network) | Loopback only, started on first use; stopped when no browser uses it for 60 s (today it lives until the server stops). Chrome cannot authenticate to SOCKS5 and cannot use a Unix socket as a proxy, so while the listener runs any local process can use it; this residual is documented | Partly designed (P2) |
| Node `/egress` | A token holder reaches the computer's LAN | Node token (D37 remote door), `EMBER_NODE_EGRESS_DENY`, `EMBER_NODE_EGRESS=off` | Built |
| Logs | Typed passwords (`text`, `key`), URLs with tokens, cookies, frame bytes | Input messages, CDP payloads and frames are never logged at any level; URLs are logged with query and fragment removed; a test feeds a marker string through `text` and `navigate?token=` and greps the captured log (`FR-R10`) | Designed (P1) |
| Profile at rest | Disk or backup theft yields live sessions | 0700 directories; no saved passwords; documented as plain text at rest; the owner's disk encryption is the control (decision O6) | Designed (P1) |
| Downloads | A page drops files that an agent or user then runs | Downloads land in the per-project downloads folder, never executed, listed with size; moving them is an explicit user act | Designed (P8) |
| Agent tools | Credential export, prompt injection | §8.4 | Designed (P6) |

## 10. Resource limits

Chrome is the heaviest process on the main server after the agents. Defaults are [provisional]
until the first measurement on the reference Mac mini and a Raspberry Pi (`NFR-RB3`).

| Limit | Default | Behaviour at the limit |
| --- | --- | --- |
| Running browsers per server | 4 (`EMBER_BROWSER_MAX`) | Opening a fifth answers 429 with the list of running ones; the least recently viewed idle one is not stopped automatically |
| Idle stop | 15 min with no viewer and no agent command (`EMBER_BROWSER_IDLE_STOP`) | Graceful `Browser.close`; reopening starts it on the same profile and egress |
| Memory per browser | Report only; 2 GiB soft (`EMBER_BROWSER_RSS_WARN`) | `ViewState.error` warns; no kill (stopping is the user's act) |
| Viewers per browser | 8 | The ninth view WebSocket is refused with 429 |
| Frame rate | 15 fps on a phone-sized `max_width`, 30 fps otherwise, via `everyNthFrame` and a server-side cap | Extra frames are acknowledged and dropped |
| Viewer send queue | 1 frame per viewer | Older frames are replaced, never queued |
| Input rate | 200 messages per second per viewer | Excess `mouseMoved` dropped; other kinds answered with `error` |
| Tabs | 20 per browser | `new_tab` refused with `error`; pages that open more get closed newest first, with a notice |
| Profile size | Report only (`GET /{project}` gains `profile_bytes`) | Shown in project settings beside "브라우저 데이터 지우기" |

## 11. Failure modes

| Failure | What the user sees | What the server does |
| --- | --- | --- |
| No Chrome on the server | Pane: "서버에 Chrome이 없습니다" with the `EMBER_CHROME_BIN` hint | `chrome: null`, start fails with that reason |
| Chrome crashes | Pane: frames stop, banner "브라우저가 종료되었습니다, 다시 시작합니다" | Restart on the same profile with backoff 1 s, 5 s, 30 s; after 3 crashes in 5 min stop and keep `error` |
| Profile locked (stale `SingletonLock`) | Nothing, if the lock is stale | Remove the lock when its pid is gone; otherwise fail with "profile in use" |
| Egress computer offline | Banner with wait or switch egress; pages show proxy errors | `egress_online=false`, never falls back to direct (`FR-R1a`) |
| Node refuses egress (403) or denies the destination | Chrome's proxy error page; banner names the node policy | `ViewState.error` from the listener's last failure |
| DNS failure on the node | Chrome's `ERR_NAME_NOT_RESOLVED` | Nothing extra: resolution is the node's, by design |
| Server restart | Pane reconnects and gets `hello` | Browsers were closed gracefully; they restart on first viewer or agent with the persisted egress |
| Slow viewer network | Lower frame rate | Frames dropped per viewer; the client lowers `max_width` when its decode lags (P5) |
| View WebSocket drops | "다시 연결하는 중" | Client reconnects with backoff; the browser keeps running for agents |
| Agent relay closed by an egress switch | The agent's next tool call reconnects | Documented; the switch notice tells the driving session (§8.3) |
| Takeover left on | Agent waits, then gets "the user has control" | Hold timeout (§8.3); takeover itself never times out |
| Disk full | Banner from Chrome's crash or the profile write error | Start fails with the OS error; nothing is deleted automatically |

## 12. Phased plan

Every phase is one or a few PRs into `develop`, each with its tests, each "Part of #12". Phases
P1 to P3 are server and protocol work that does not wait for a decision; P4 onward need O1.

| Phase | PR-sized steps | Requirements | Tests |
| --- | --- | --- | --- |
| **P0** (done) | Server instance, profile, egress, view stream, input, relay, takeover, agent config | `FR-R1`–`FR-R4` | `tests/browser.rs`, `tests/browser_egress.rs` |
| **P1** Security baseline | (a) Browser routes behind D37: tests that the view, input, egress, data and plain `cdp` routes answer 401 without the token and 403 on a foreign Origin. (b) `--remote-debugging-pipe` instead of a port, `debug_port` removed from `BrowserInfo`. (c) Profile dirs 0700, password saving off, project deletion removes the profile. (d) Log hygiene test | `FR-R7`, `FR-R10` | Server integration tests; the log test captures `tracing` output |
| **P2** Lifecycle and limits | (a) Max browsers, idle stop, viewers cap, input rate. (b) Crash restart with backoff and stale lock removal. (c) Egress listener stop after idle, `egress_online` from the registry. (d) CI guard: the Linux test job fails if no Chrome is found, so `tests/browser.rs` cannot pass by skipping | `FR-R1a`, `FR-R11`, `FR-R12` | Integration tests with a fake slow viewer and a killed Chrome pid |
| **P3** Client protocol | `ember_client::browser`: view WebSocket client, frame header parser, coordinate mapping, input encoder, reconnect. Shared vectors in `ember/vectors/browser/` (frame bytes, input JSON) used by server and client tests | `FR-R5a` | Unit tests on vectors; a client-against-real-server test where Chrome exists |
| **P4** Viewer v0 | Browser pane in the 세션 screen behind `EMBER_CR_BROWSER=1`: frame surface by asset swap, toolbar, status line. Measurement probe for fps, decode time, RSS | `FR-R5a`, `FR-R6`, `NFR-RB1`, `NFR-RB2` | Pure model tests (`fr_r5_*`), the memory gate (1000 frames, flat RSS), `check-no-webview.sh` |
| **P5** Input | Pointer, wheel, keys, the text field for IME, paste, tabs, touch | `FR-R5b`, `FR-R5c` | Model tests for the coordinate and key mapping; server test that `touch` reaches `touchstart` in a page |
| **P6** Agents | Per-project grant, per-session relay nonce, agent identity in `ViewState`, takeover hold timeout, end-of-takeover notice, CDP deny list, pinned MCP packages | `FR-R3a`, `FR-R8`, `FR-R9` | Relay tests with a fake MCP client: held, timed out, denied method, notice delivered |
| **P7** Egress UX | Egress chip and chooser in the pane, default to the session's computer, offline banner, 웹 페이지 menu "Ember 브라우저에서 열기" | `FR-R1a`, `FR-R13` | Model tests; server test that a first open with a session sets that computer |
| **P8** Downloads and project cleanup | Download folder, list, file transfer hand-off; profile size | `FR-R14` | Server test downloading a file from the tiny test site |
| **P9** compose-rust frame surface (only if P4 misses NFR) | Upstream issue with P4's numbers; Ember swaps the widget | `NFR-RB1`, `NFR-RB2` | P4's probe rerun |
| **P10** Acceptance | The #12 completion criteria on real hardware: what-is-my-IP through a second computer, its `localhost` reachable, login survives an egress switch, an agent fills a form while the user takes over a login, from the desktop app and the phone | `FR-R1`–`FR-R5` | Manual, recorded in SPEC with the date and machines; not verifiable in CI |

## 13. Test strategy

- **Server, real Chrome.** `tests/browser.rs` already runs Chrome against a tiny local site and a
  test SOCKS5 server. It skips without Chrome, so P2 adds a guard that makes the Linux CI job fail
  when no Chrome is found (GitHub's Ubuntu images ship one). New behaviour (limits, crash restart,
  takeover timeout, deny list) is tested the same way.
- **Server, no Chrome.** Relay and limit logic that does not need a page is tested against a fake
  CDP endpoint (a WebSocket that answers `Target.*` and records methods), so it runs everywhere.
- **Egress.** The pieces are tested separately today (node SOCKS5 over `/egress`, listener to node
  to a TCP echo, persistence). P2 adds one test that runs Chrome through a real `ember-node` started
  in-process and checks that a page on the node's loopback is reachable.
- **Protocol vectors.** Frame encoding and input JSON live in `ember/vectors/browser/` and are
  checked by both server and client tests, so the two cannot drift.
- **Client.** Pure models (coordinate mapping, key mapping, reconnect, state to banner text) are
  unit-tested in `ember/app/src/ui/browser.rs`; the compose-rust pane is tested the way the other
  `ui/cr/` screens are. `check-no-webview.sh` stays the E1 gate.
- **Security.** Each row of §9 that is marked built or designed has a test named after its
  requirement (`fr_r7_view_requires_token`, `fr_r10_input_text_is_not_logged`, ...).
- **Measurements.** `NFR-RB*` numbers are recorded next to the requirement with the machine, the
  build and the date, as the repository requires for performance claims.
- **Hardware.** The `FR-R1`–`FR-R5` acceptance runs need two computers, a real network and a
  phone; they are reported as not verified until a person runs them (P10).

## 14. Owner decisions

| ID | Question | Options | Recommendation |
| --- | --- | --- | --- |
| **O1** | Where does the browser appear in the client? | (a) A pane type in the 세션 screen (multi view pane, single view right panel tab, full screen on a phone). (b) A separate native window, like the Ember 에디터. (c) Both | **(a)** first: it works on every client, including the phone (`FR-R5`), and needs no new window code. (b) can follow as "pop out" |
| **O2** | Which computer does the browser egress from, and does it follow session moves? | (a) Fixed per project, defaulting to the opening session's computer (this design). (b) Follow the session's current computer, including D20 fallbacks to the main server. (c) Today's behaviour: direct until someone chooses | **(a)**: a site that sees the IP change mid-login may lock the account, and the user picked that vantage point. A lost computer pauses the browser instead of switching to direct |
| **O3** | What is the unit of a browser identity? | (a) Per project (built). (b) Per session. (c) Per project, with per-session contexts sharing nothing | **(a)**: D6 asks for one identity; members of a project share its logins |
| **O4** | How are agents allowed to drive the browser? | (a) Server-wide env only (today). (b) Per project grant under the server switch (this design). (c) Per session grant | **(b)**: the risk (§8.4) is about which logins an agent can reach, and logins are per project |
| **O5** | Do we block CDP methods that export cookies? | (a) No filter (residual risk documented). (b) Deny list in the relay (this design). (c) Agents get a separate browser context without the user's logins | **(b)**: cheap, keeps the off-the-shelf MCP servers, and removes the one-call cookie dump; (c) defeats "the agent continues after the user's login" |
| **O6** | How is the profile protected at rest? | (a) 0700, no saved passwords, rely on the owner's disk encryption. (b) Use the OS keychain (drop `--password-store=basic`): no keyring on a headless Linux server. (c) Ember encrypts the profile directory when the browser stops | **(a)** now, documented; (c) only if the owner wants backups of the data folder to be safe on their own |
| **O7** | Which pixel transport? | (a) JPEG screencast (built). (b) A video codec over the same WebSocket | **(a)**; revisit only if measured bandwidth, not decode, misses `NFR-RB1` |
| **O8** | What does the 웹 페이지 menu open? | (a) The system browser only (today). (b) The Ember browser only. (c) Both, per row | **(c)**: the system browser is faster for LAN ports; the Ember browser is the only way to a remote computer's loopback port, and it is the "Ember 터널" the mockup promises |
| **O9** | Third-party code and calls | (a) Pin chrome-devtools-mcp and Playwright MCP versions, fetched by npx. (b) Vendor them into the release. Plus: is an IP echo service allowed for the egress check, and which one? | **(a)** pinned, updated by PR; the IP echo off by default and configured by the owner |
| **O10** | Requirement IDs | `NFR-R1` is a release requirement, so browser NFRs cannot be `NFR-R2…` without confusion | Use `NFR-RB1…` for the browser (this design) and leave the release ID alone |

## 15. Proposed INTENT change (not applied)

D6 stays as it is. The open note under SPEC §R ("How the browser is rendered to the client without
breaking E1 … is open") and PROJECT.md's M6 open question are answered by this design, so a new
decision is proposed for the owner to confirm, to be written into `docs/INTENT.md` only after
confirmation:

> **D6a: The remote browser is shown as streamed pixels in a native pane.** The server renders; the
> client paints JPEG frames in a compose-rust surface and forwards input. No webview is used, in
> any screen, for the remote browser. Rejected: a webview window for the browser (it would show a
> picture anyway, or run the page locally and lose the server profile and the egress); the user's
> system browser through a forwarded port (it loses the server-held profile).

PROJECT.md's M6 row says "viewing depends on dioxus-compose's frame-surface widget". That widget
does not exist in dioxus-compose; v0 needs only compose-rust's image assets, and the frame surface
is a compose-rust request (P9). The row is updated when D6a is confirmed.
