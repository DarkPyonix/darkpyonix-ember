# Remote browser: server side (M6, SPEC §R, INTENT D6)

Code: `crates/server/src/browser/`. Stream protocol version: **1** (`browser::STREAM_VERSION`).

## Model

- One headless Chromium per project runs on ember server (`--headless=new`, 1280×800 viewport).
- Profile: `<EMBER_DATA_DIR>/browser/<project>/profile` (FR-R2). `DELETE …/data` wipes it (FR-R4).
- Egress (FR-R1): chosen per project: `{"kind":"direct"}`, `{"kind":"proxy","url":…}`
  (`socks5://`, `socks4://`, `http://`, `https://`; no credentials) or `{"kind":"computer","id":…}`
  (a registered computer; `local` = direct). A computer resolves, at every Chrome start, to that
  computer's loopback SOCKS5 listener in ember server (below). Loopback is sent through the proxy
  too (`--proxy-bypass-list=<-loopback>`), so the egress computer's `localhost` is what pages
  reach. A computer that cannot be resolved (removed, node URL invalid) makes the start fail; it
  never falls back to direct. Changing egress restarts Chrome on the same profile after a graceful
  `Browser.close` (cookies flushed), so logins survive (FR-R2).
- The choice is **persisted** in `ember.db` (`browser_egress(project, kind, value, updated_at)`,
  store migration 5) and reapplied when the project's browser is next started after a server
  restart. A computer some project egresses through cannot be removed (409).
- Browser binary: `EMBER_CHROME_BIN`, else Google Chrome / Chromium / Chrome for Testing app bundles,
  `google-chrome`/`chromium` on `PATH`, else the newest Playwright-cached Chromium.

## HTTP routes (under `/api/v1/browsers`)

| Route | Body / result |
| --- | --- |
| `GET /` | `{v, chrome, browsers: [BrowserInfo]}` |
| `GET /{project}` | `BrowserInfo` (404 if never opened) |
| `POST /{project}` | `{egress?: string\|null, computer?: id}`: start or keep running; either given → switch to it (persisted) |
| `GET /{project}/egress` | `{egress: Egress, proxy: string\|null, running}`: the persisted choice and the proxy Chrome last started with |
| `PUT /{project}/egress` | `{egress: string\|null}` or `{computer: id}`: switch egress (restart, same profile; persisted). Both → 400 |
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

- **Text** `{"type":"hello","v":1,"project":…,"state":ViewState}`: first message. A client that
  does not know `v` must say so to the user, not guess.
- **Text** `{"type":"state","v":1,"project":…,"state":ViewState}`: on every state change.
- **Text** `{"type":"error","v":1,"message":…}`: an input message failed.
- **Binary** frame: `[u32 big-endian header length N][N bytes UTF-8 JSON header][JPEG bytes]`.
  Header: `{"type":"frame","v":1,"seq","format":"jpeg","target_id","metadata","agent_active","takeover"}`;
  `metadata` is CDP's `ScreencastFrameMetadata` (`deviceWidth`, `deviceHeight`, `pageScaleFactor`,
  `offsetTop`, `scrollOffsetX/Y`, `timestamp`). Frames may be dropped for a slow viewer; each frame
  is a full image, so the client just shows the latest.

`ViewState = {running, egress, egress_computer, target_id, url, title, tabs: [{target_id,url,title}], agent_active,
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
| `reload` / `back` / `forward` | (none) | `Page.reload` / `history.back()` / `history.forward()` |
| `takeover` | `on` | user takes control: agent commands are held until `on: false` |
| `select_tab` | `target_id` or `null` (follow the newest tab) | re-attach the stream |
| `screencast` | `quality?`, `max_width?`, `max_height?` | restarts `Page.startScreencast` |

`modifiers` is CDP's bit field: Alt 1, Ctrl 2, Meta 4, Shift 8.

## Egress through a computer (FR-R1)

```text
Chrome --proxy-server=socks5://127.0.0.1:<port>
  └─TCP─▶ ember server: loopback listener for computer X (crates/server/src/computers/egress.rs)
            └─ one WebSocket per TCP connection: GET <node>/v1/egress (Bearer <node token>)
                 └─▶ ember node X: SOCKS5 server (crates/node/src/egress.rs) ──TCP─▶ target
```

- The server listener does not parse SOCKS5; it copies bytes, so Chrome's SOCKS5 client and the
  node's SOCKS5 server talk end to end, and domain names are resolved **on the node** (Chrome
  sends hostnames to SOCKS5 proxies). Listener: one per computer, started on first use, bound to
  127.0.0.1 without authentication (Chrome cannot authenticate to SOCKS5), kept until the
  computer is removed or the server stops. Its port changes per server run, which is why the
  computer id (not the URL) is persisted.
- `/v1/egress` stream format: a raw SOCKS5 (RFC 1928) byte stream in Binary frames. SOCKS5
  auth over this route is "no authentication" (the upgrade carried the node token). `CONNECT`
  only (others → reply `0x07`); IPv4, IPv6 and domain addresses. **An empty Binary frame is a
  half-close** (TCP FIN) in that direction; a side that has both sent and received one closes
  the WebSocket; Close ends both directions.
- Node policy: default allow everything (the computer's `localhost` and LAN must be reachable).
  `EMBER_NODE_EGRESS_DENY=private,link-local,loopback,<CIDR>,…` denies destinations (every
  resolved address is checked; none left → reply `0x02`); `EMBER_NODE_EGRESS=off` refuses
  `/v1/egress` with 403.
- Optional plain SOCKS5 on the node: `EMBER_NODE_SOCKS_LISTEN=<addr>`; loopback clients need no
  auth, others RFC 1929 username/password with the node token as password.
- Today the node stream is a WebSocket over the node's plain HTTP; with the transport (FR-N5) it
  becomes a transport stream. One stream per proxied TCP connection, so each new connection pays
  a WebSocket handshake to the node.

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

Opt-in per server: `EMBER_BROWSER_MCP=chrome-devtools|playwright|off` (default **off**). The MCP
server is run by `npx -y <pkg>` or, without npx, `bunx <pkg>` (found on `PATH` and passed as an
absolute path; `EMBER_BROWSER_MCP_RUNNER` names one); with no runner the setting is ignored with
a warning. When on, a start-config hook adds the server to every agent start
(`StartRequest::mcp_servers`) and exports `EMBER_BROWSER_CDP_WS` / `EMBER_BROWSER_MCP_CONFIG`; the
adapters turn it into flags:

- Claude Code: `--mcp-config=<{"mcpServers":{"ember-browser":{"type":"stdio","command","args"}}}>`
  (equals form, because `--mcp-config <configs...>` is variadic in claude 2.1.288), plus
  `MCP_TIMEOUT=90000` unless the server environment sets it (first `npx` run downloads the
  package).
- Codex: `codex app-server -c mcp_servers.ember-browser.command="…" -c
  mcp_servers.ember-browser.args=[…] -c mcp_servers.ember-browser.startup_timeout_sec=90`
  (`codex app-server --help`, codex-cli 0.155.1: `-c key=value`, value parsed as TOML).

The relay opens one upstream DevTools connection per agent connection and passes messages
unchanged. Each agent→browser message sets `agent_active` (true for 3 s after the last command) and,
while the user has taken over, waits until takeover ends, so the agent pauses and then continues
(FR-R3). An egress switch restarts Chrome and closes relay connections; MCP servers reconnect on
their next tool call.

## Open

- FR-R5 mobile: the protocol is plain WebSocket + JPEG, usable from any client; touch is mapped to
  `mouse` by the client for now (no `Input.dispatchTouchEvent`), and there is no per-viewer viewport
  (one shared 1280×800 viewport; `screencast.max_width` reduces bandwidth only).
- The raw Chrome DevTools port (`debug_port`) is unauthenticated on 127.0.0.1; agents should use the
  relay. The ember API itself has no auth yet.
- Egress through a computer has not been run with a real Chrome against a real node; the
  pieces are tested separately (node SOCKS5 over `/v1/egress`, server listener → node → TCP echo,
  persistence across a manager restart).
- Codex sessions on another computer (`environment/add`): whether Codex starts stdio MCP servers
  locally (where the relay is reachable) or in the remote environment is unverified.
- The browser listener is unauthenticated on loopback: any local process on ember server can use a
  computer's egress while its listener runs.
