# Remote browser — server side (M6, SPEC §R, INTENT D6)

Code: `server/src/browser/`. Stream protocol version: **1** (`browser::STREAM_VERSION`).

## Model

- One headless Chromium per project runs on ember server (`--headless=new`, 1280×800 viewport).
- Profile: `<EMBER_DATA_DIR>/browser/<project>/profile` (FR-R2). `DELETE …/data` wipes it (FR-R4).
- Egress: one proxy URL per browser (`socks5://`, `socks4://`, `http://`, `https://`; no credentials).
  Loopback is sent through the proxy too (`--proxy-bypass-list=<-loopback>`), so the egress computer's
  `localhost` is what pages reach (FR-R1). Changing egress restarts Chrome on the same profile after a
  graceful `Browser.close` (cookies flushed), so logins survive (FR-R2). Later the proxy URL will
  point at a local listener that carries SOCKS5 to the ember node over the transport (FR-N5).
- Browser binary: `EMBER_CHROME_BIN`, else Google Chrome / Chromium / Chrome for Testing app bundles,
  `google-chrome`/`chromium` on `PATH`, else the newest Playwright-cached Chromium.

## HTTP routes (under `/api/v1/browsers`)

| Route | Body / result |
| --- | --- |
| `GET /` | `{v, chrome, browsers: [BrowserInfo]}` |
| `GET /{project}` | `BrowserInfo` (404 if never opened) |
| `POST /{project}` | `{egress?: string\|null}` — start or keep running; `egress` present → switch to it |
| `PUT /{project}/egress` | `{egress: string\|null}` — switch egress (restart, same profile) |
| `DELETE /{project}` | stop (graceful) |
| `DELETE /{project}/data` | clear profile; a running browser restarts empty with the same egress |
| `POST /{project}/input` | one input message (below); 409 if no page is attached |
| `GET /{project}/view` | WebSocket: the view stream |
| `GET /{project}/cdp` | WebSocket: DevTools relay for agents |
| `GET /{project}/cdp/json/version` | DevTools discovery doc; `webSocketDebuggerUrl` = the relay |
| `GET /{project}/agent-config?mcp=chrome-devtools\|playwright` | MCP config for Claude Code / Codex |

`BrowserInfo = {project, profile_dir, debug_port, state: ViewState}`.

## View stream (WebSocket `GET /api/v1/browsers/{project}/view`), v1

Connecting starts the browser if needed and registers a viewer; the screencast runs only while at
least one viewer is connected. All viewers share one stream (one tab, one quality setting).

### Server → client

- **Text** `{"type":"hello","v":1,"project":…,"state":ViewState}` — first message. A client that
  does not know `v` must say so to the user, not guess.
- **Text** `{"type":"state","v":1,"project":…,"state":ViewState}` — on every state change.
- **Text** `{"type":"error","v":1,"message":…}` — an input message failed.
- **Binary** frame: `[u32 big-endian header length N][N bytes UTF-8 JSON header][JPEG bytes]`.
  Header: `{"type":"frame","v":1,"seq","format":"jpeg","target_id","metadata","agent_active","takeover"}`;
  `metadata` is CDP's `ScreencastFrameMetadata` (`deviceWidth`, `deviceHeight`, `pageScaleFactor`,
  `offsetTop`, `scrollOffsetX/Y`, `timestamp`). Frames may be dropped for a slow viewer; each frame
  is a full image, so the client just shows the latest.

`ViewState = {running, egress, target_id, url, title, tabs: [{target_id,url,title}], agent_active,
agent_connections, takeover, viewers, error}`. Additive fields do not bump `v`.

### Client → server (text JSON; same shapes for `POST …/input`)

Coordinates are CSS pixels in the viewport, i.e. the `deviceWidth`×`deviceHeight` space of the frame
metadata; the JPEG may be downscaled (`max_width/height`), so clients scale by
`deviceWidth / jpegWidth`.

| `type` | Fields | CDP |
| --- | --- | --- |
| `mouse` | `event` (`mousePressed`/`mouseReleased`/`mouseMoved`), `x`, `y`, `button` (`left`/`middle`/`right`/`none`), `click_count`, `modifiers` | `Input.dispatchMouseEvent` |
| `wheel` | `x`, `y`, `delta_x`, `delta_y`, `modifiers` | `Input.dispatchMouseEvent` (`mouseWheel`) |
| `key` | `event` (`keyDown`/`keyUp`/`rawKeyDown`/`char`), `key`, `code`, `text?`, `key_code?` (Windows VK), `modifiers` | `Input.dispatchKeyEvent` |
| `text` | `text` | `Input.insertText` (IME, mobile keyboards) |
| `navigate` | `url` | `Page.navigate` |
| `reload` / `back` / `forward` | — | `Page.reload` / `history.back()` / `history.forward()` |
| `takeover` | `on` | user takes control: agent commands are held until `on: false` |
| `select_tab` | `target_id` or `null` (follow the newest tab) | re-attach the stream |
| `screencast` | `quality?`, `max_width?`, `max_height?` | restarts `Page.startScreencast` |

`modifiers` is CDP's bit field: Alt 1, Ctrl 2, Meta 4, Shift 8.

## Agents (FR-R3)

Agents run on ember server next to the browser and get an unmodified, off-the-shelf DevTools MCP
server pointed at the relay `ws://127.0.0.1:<port>/api/v1/browsers/<project>/cdp`:

- chrome-devtools-mcp (default): `npx -y chrome-devtools-mcp@latest --wsEndpoint=<relay>`
- Playwright MCP: `npx -y @playwright/mcp@latest --cdp-endpoint=<relay>`

Both attach to an existing browser rather than launching one. Configuration only, no patching (E2):

- **Claude Code**: `claude … --mcp-config '<agent-config.claude_code_mcp_config JSON>'`
  (`--mcp-config` accepts JSON strings; without `--strict-mcp-config` the user's own servers stay).
- **Codex**: `codex app-server -c 'mcp_servers.ember-browser.command="npx"' -c 'mcp_servers.ember-browser.args=[…]'`
  (`agent-config.codex_overrides`), or the TOML section in `codex_toml`.

A session start hook exports `EMBER_BROWSER_CDP_WS` and `EMBER_BROWSER_MCP_CONFIG` to every agent
process. Not wired yet: the adapters turning these into the flags above (a few lines in
`agents/claude_code.rs` `args()` and `agents/codex.rs` spawn).

The relay opens one upstream DevTools connection per agent connection and passes messages
unchanged. Each agent→browser message sets `agent_active` (true for 3 s after the last command) and,
while the user has taken over, waits until takeover ends — so the agent pauses and then continues
(FR-R3). An egress switch restarts Chrome and closes relay connections; MCP servers reconnect on
their next tool call.

## Open

- FR-R5 mobile: the protocol is plain WebSocket + JPEG, usable from any client; touch is mapped to
  `mouse` by the client for now (no `Input.dispatchTouchEvent`), and there is no per-viewer viewport
  (one shared 1280×800 viewport; `screencast.max_width` reduces bandwidth only).
- The raw Chrome DevTools port (`debug_port`) is unauthenticated on 127.0.0.1; agents should use the
  relay. The ember API itself has no auth yet.
- Egress is not persisted across server restarts.
