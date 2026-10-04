# DarkPyonix: the VS Code Web wrapping layer

A reverse proxy that makes VS Code Web (`code serve-web`) usable on tablets and phones.
It serves the home launcher and the login page itself, and wraps a workspace in an iframe
so that a parent page owns the navigation bar. That bar is turned on and off by window
width, so on a wide screen you see stock VS Code, untouched.

In Ember's vocabulary this is the wrapping layer of `FR-W2`: CSS/DOM overrides layered on
an official, unmodified VS Code Web build. No upstream source is patched.

---

## ⚠️ There are two processes

`code serve-web` is **stock VS Code Web and nothing else**.
**Every screen we built (the bar, home, login, the wrapper) lives in the proxy (:8888).**
Going directly to the serve-web port and seeing nothing is the expected behavior.

```
browser ──▶ :8888  DarkPyonix (main.py)  ──▶ :9094   code serve-web
                   home · login · frame wrapper       stock VS Code Web
                   CSS/JS injection · WS relay
```

Always connect to **http://localhost:8888/**.

---

## First run on a new machine

The repository holds **source only**. Local state such as the database and the recent list
is in `.gitignore` and is created automatically on first run.

```powershell
Set-Location <the folder you cloned into>

# 1) uv (https://docs.astral.sh/uv/). Python 3.10 or newer is required; this fetches one
uv python install 3.12

# 2) The first account: without this there is no account, so nothing can be opened
$env:DPX_USERNAME="<user>"; $env:DPX_PASSWORD="<password>"
```

⚠️ **No default account is created**: both variables must be set. Login is still a thin
shell (see [docs/BACKGROUND.md](docs/BACKGROUND.md)). The variables apply exactly once, when
`darkpyonix.db` does not yet exist, so if the account already exists use
`/auth/change-password` or delete the database and start again.

That machine also needs `code serve-web` (it ships with VS Code).

⚠️ **Using this from a phone or tablet requires HTTPS, and how to obtain it is undecided**;
see the section below.

---

## Running it

There is no separate install step: `uv run --with-requirements requirements.txt` resolves the
dependencies on first use and caches them.

Every time, in two terminals:

```powershell
# 1) Upstream VS Code Web
code serve-web --host 127.0.0.1 --port 9094 --without-connection-token --accept-server-license-terms

# 2) The proxy: the port above must be passed as XMO_UPSTREAM_PORT (the default is 9092)
$env:XMO_UPSTREAM_PORT="9094"; uv run --with-requirements requirements.txt uvicorn main:app --host 0.0.0.0 --port 8888
```

If a port is stuck `LISTENING` without responding (a dead serve-web), move to the next port.
Check with: `netstat -ano | findstr LISTENING | findstr 909`

### 🔴 Serving over HTTPS is mandatory for phones and tablets

**VS Code Web's webviews (extension panels: Claude Code, Codex, …) depend on service
workers, and service workers only run in a secure context (HTTPS or localhost).** Over plain
HTTP with a LAN IP, `isSecureContext=false` → `navigator.serviceWorker` does not exist →
**every webview is blank.** (Measured: `http://127.0.0.1:8888` → secure=true, and
`http://<LAN IP>:8888` → secure=false.)

**🚧 The method is not decided yet.** The self-signed certificates used previously (`certs/`
plus a generation script) were **abandoned**: they were pinned to a LAN IP, so they had to be
reissued whenever the network or the machine changed, every visit required clicking through a
warning, and the private key ended up in the repository.

So **HTTPS is currently unavailable, and extension webviews are blank on real devices.**
`localhost` on the laptop still works.

The candidates (Tailscale, Cloudflare Tunnel, DDNS + Let's Encrypt, mkcert) and the reasoning
are collected in **[docs/BACKGROUND.md](docs/BACKGROUND.md)**. Once decided, this section gets
rewritten around that method.

Because the proxy preserves the client's Host header, **no code has to change whichever method
is chosen** (see the "tablet lockup" entry in [docs/CONSTRAINTS.md](docs/CONSTRAINTS.md)).

Health check: `curl http://127.0.0.1:8888/healthz` → `ok`

## Running it next to ember node (`python -m dpx.serve`)

This is how the IDE window runs on each computer (SPEC `FR-W1`). One command, no prompts:
it checks the VS Code runtime, installs the default extensions once, starts the runtime's
web server on a free **loopback** port, starts the proxy in front of it with **folder roots
enforced**, and prints one JSON line when both answer:

```sh
cd web/proxy
export DPX_USERNAME=<user> DPX_PASSWORD=<password>     # first run only, as above
uv run --with-requirements requirements.txt python -m dpx.serve --runtime vsc --root ~/work --root ~/src
# {"event": "ready", "url": "http://127.0.0.1:53817/", "upstream_port": 53816, "roots": [...], ...}
```

Logs go to stderr; stdout carries only the ready line (also written to `--announce-file`
if given, and removed on exit). Ctrl-C or SIGTERM stops both processes. Exit codes: `2`
bad arguments (no root, a root that is not a directory), `3` runtime not installed,
`4` a process failed to start or died.

**Runtimes** (`INTENT.md` D10, `FR-W4`): `--runtime` or `DPX_RUNTIME`:

| Runtime | Default | Server | Configure |
|---|---|---|---|
| `ose` | yes | DarkPyonix-built Code-OSS web server (Open VSX) | `DPX_OSE_SERVER` = its launcher; `DPX_OSE_ARGS` = argument template with `{host}` `{port}` `{data_dir}` (default: the REH web server's `--host --port --without-connection-token --accept-server-license-terms --server-data-dir`) |
| `vsc` | no | the user's Microsoft VS Code, `code serve-web` | `DPX_CODE_BIN` (default `code` on `PATH`) |

`python -m dpx.serve --runtime vsc --check` prints the runtime status as JSON
(`installed`, `version`, `commit`, `arch`, and `install_guide` when missing), the data
behind INTEGRATION.md's "verify `code --version`" step. A running proxy serves the same at
`GET /__runtime` (needs a session).

**Folder roots.** At least one `--root` (or `DPX_FOLDER_ROOTS`, `os.pathsep`-separated) is
required in this mode. A `?folder=` or `?workspace=` outside every root (after resolving
`..` and symlinks) is answered `403` before it reaches serve-web or the recent list. This
restricts which workspace a URL opens; it is **not** a filesystem sandbox (the extension host
and terminal still run as the OS user). The standalone proxy honours `DPX_FOLDER_ROOTS` too,
and with it unset keeps its old, unrestricted behaviour.

**Default extensions** (`FR-W6`). Each entry of `--extension` / `DPX_DEFAULT_EXTENSIONS`
(a marketplace id or a `.vsix` path) is installed into `<data-dir>/extensions`, the directory
the server started with `--server-data-dir <data-dir>` loads from; an entry already there is
skipped. The default is the DarkPyonix theme, `darkpyonix.vscode-darkpyonix-theme`, **not
published yet**, so until it is, its install fails with a warning (the server still starts);
point `DPX_DEFAULT_EXTENSIONS` at the `.vsix`, or set it empty. `--data-dir` /
`DPX_SERVER_DATA_DIR` defaults to `~/.ember/vscode-web/<runtime>`, one per runtime so the two
marketplaces never mix; settings there are separate from a desktop VS Code's.

| Option | Env | Default |
|---|---|---|
| `--root DIR` (repeatable) | `DPX_FOLDER_ROOTS` | required |
| `--runtime ose\|vsc` | `DPX_RUNTIME` | `ose` |
| `--server CMD` | `DPX_OSE_SERVER` / `DPX_CODE_BIN` | per runtime |
| `--host` | `DPX_SERVE_HOST` | `127.0.0.1` |
| `--port` | `DPX_SERVE_PORT` | `0` = a free port |
| `--public-url` | `DPX_PUBLIC_URL` | `http://<host>:<port>/` (a wildcard bind is announced as `127.0.0.1`) |
| `--data-dir` | `DPX_SERVER_DATA_DIR` | `~/.ember/vscode-web/<runtime>` |
| `--extension ID_OR_VSIX` (repeatable) | `DPX_DEFAULT_EXTENSIONS` | the DarkPyonix theme |
| `--announce-file PATH` | (none) | none |

The URL it announces is what ember server's "Open IDE" returns as the `vscode` target
(`EMBER_IDE_COMPUTERS` → `ide_url`, see `crates/server/src/api/ide.rs`).

Tests (stdlib only; the middleware test is skipped without FastAPI):
`python3 -m unittest discover -s tests -t .`

---

## Screen flow

```
/                     home launcher: workspace cards
  └─ [Open] ──▶ /login?...        login page
        └─ POST /auth/login ──▶ /?folder=<path>   workspace
```

- Workspaces (`/?folder=...`) and the serve-web assets and WebSocket **require a valid
  session**. Requesting HTML without one returns a 303 redirect to
  `/login?next=<original address>`.
- **There is no initial account**: one is created only on first start, and only if
  `DPX_USERNAME` and `DPX_PASSWORD` are both set. Without an account, the startup log warns
  and logging in is impossible.
- ⚠️ **An account is an entry pass, not an identity**: there is no per-user file isolation,
  so whoever logs in sees the files of the OS account that started the server. Using one
  account from a laptop, a phone and a tablet at the same time is supported on purpose. See
  [docs/BACKGROUND.md](docs/BACKGROUND.md).
- The workspace screen switches on window width: wide gives stock VS Code, narrow or touch
  gives our bar. The switch happens **without a reload**.

---

## Configuration (environment variables)

| Variable | Default | Description |
|---|---|---|
| `XMO_UPSTREAM_HOST` | `127.0.0.1` | serve-web host |
| `XMO_UPSTREAM_PORT` | `9092` | serve-web port: **must be set if you started it on another port** |
| `DPX_USERNAME` | (none) | Name of the first account: **both variables are required to create it** |
| `DPX_PASSWORD` | (none) | Password of the first account. Without it, the server starts with no account |
| `DPX_NO_ASSET_CACHE` | (none) | `1` re-reads `static/` on every request (for working on the screens) |
| `DPX_TAB_DETACH` | `1` | `0` stops injecting `detach.js` and the `dragToOpenWindow` default → stock tab drag (see "Tab detach") |
| `DPX_HOME` | user home | Where to look for agent session files (`~/.claude`, …) |
| `DPX_HUB_URL` · `DPX_HUB_TOKEN` | (none) | If set, the hub connector starts alongside → [docs/HUB.md](docs/HUB.md) |
| `DPX_FOLDER_ROOTS` | (none) | Folders `?folder=` may open (`os.pathsep`-separated); outside → 403. Unset = unrestricted |
| `DPX_RUNTIME` · `DPX_CODE_BIN` · `DPX_OSE_SERVER` | `ose` · `code` · (none) | The runtime `/__runtime` reports on (set by `dpx.serve`) |

---

## Notes for development

- **Editing a `.py` file requires restarting the proxy** (when started without `--reload`).
  Injected CSS/JS is served no-cache, so a refresh after the restart is enough.
- If a workspace tab was open, **closing it completely and reopening** is the safe move. A
  service worker holding old assets looks exactly like a regression that is not there.
- To work on the screens only (HTML/CSS/JS) you never need to open `main.py`. It is all in
  `static/` → [static/README.md](static/README.md). Starting with
  `$env:DPX_NO_ASSET_CACHE="1"` makes a refresh enough, with no restart.
- The injected JS is real `.js`, so it syntax-checks as-is. Before committing:
  ```powershell
  node --check static\overlay.js; node --check static\webview-kb.js; node --check static\detach.js
  ```
  The `<script>` inside `frame.html` is still inline, so check that one in the browser console.
- To verify layout with browser automation, install Playwright separately (it is not a runtime
  dependency, so it is not in `requirements.txt`):
  `uv run --with playwright playwright install chromium`, then run the scripts with
  `uv run --with-requirements requirements.txt --with playwright python <script>`.
  ⚠️ **Headless Chromium cannot render extension webviews at all**: only geometry
  (coordinates, styles) can be verified there, and whether a webview actually appears is only
  confirmed on a real device.

---

## URL parameters

The normal path is `/?folder=<path>` and **nothing else**. Whether to wrap and whether to show
the bar are decided automatically.

The development bypass switches (`xmo=on`, `xmo=off`, `xmodebug`, `xmojs`, `xmokb`, `xmofs`)
and the legacy overlay mode have **all been removed**. There is no switch left to touch.

`xmo=embed`, `xmochrome` and `xmoorient` still exist, but they are not switches; they are
**internal values the wrapper passes to its own iframe**. Do not use them directly.

---

## Endpoints

| Path | Description |
|---|---|
| `/` | Home launcher (no `folder`) / workspace wrapper (`?folder=`) |
| `/login` | Login page |
| `/auth/login` · `/logout` · `/me` | Auth API (session cookie `dpx_session`) |
| `/auth/users` · `/auth/change-password` | User management |
| `/__overlay.css` · `/__overlay.js` | Overlay assets injected into the workbench top-level document |
| `/__kb.js` | Keyboard policy injected into VS Code webview frames (extension panels) only |
| `/__detach.js` | Tab detach, injected into the workbench top-level document next to the overlay |
| `/__workspaces` | Recent workspaces the server remembers (`_xmo_recent.json`) |
| `/__agents/*` | Agent conversation API, used by the home screen → [AGENTS.md](AGENTS.md) |
| `/healthz` | Health check → `ok` |
| `/__runtime` | VS Code runtime status (installed, version, install guide) |
| `/__ext/ping` · `/__ext/state` | Extension-gating receiver: **plumbing only, unused** |
| everything else | Proxied to serve-web (including WebSocket) |

---

## Files

`main.py` used to hold the HTML, CSS, JS, API and proxy all at once; it has been split up.
**To change a screen open `static/`; to change behavior open one folder under `dpx/`.**

```
web/proxy/
├─ main.py            app assembly + request routing ← read only this for the overall flow
├─ dpx/
│  ├─ serve.py        `python -m dpx.serve`: runtime server + proxy, one entry
│  ├─ config.py       env vars · paths · constants (every setting lives here)
│  ├─ assets.py       serving static/ files
│  ├─ auth/           authentication
│  │  ├─ gate.py        which paths are open without a session
│  │  └─ api.py         the /auth login app: users · sessions · SQLite
│  ├─ vscode/         everything on the VS Code Web side
│  │  ├─ proxy.py       upstream relay (HTTP streaming · WebSocket)
│  │  ├─ inject.py      where the overlay CSS/JS gets injected
│  │  ├─ html_rewrite.py  the pure workbench-document rewrites (testable without FastAPI)
│  │  ├─ roots.py       folder roots (`?folder=` restriction)
│  │  ├─ runtime.py     OSE / VSC runtime check · server command · default extensions
│  │  └─ extension.py   companion-extension heartbeat gate + /__ext/*
│  ├─ terms/          ember node's persistent terminals relayed as /__terms/* (SPEC §P)
│  ├─ home/           the back end of the home screen
│  │  ├─ api.py         /__agents/* · /__workspaces
│  │  └─ workspaces.py  the record of recently opened folders
│  ├─ agents/         conversation adapters (Claude Code · Codex) → AGENTS.md
│  └─ hub/            several machines on one home screen (optional) → docs/HUB.md
│     ├─ server.py
│     └─ connector.py
├─ companion/         VS Code web extension: integrated terminals as ember node sessions
│                      (docs/design/TERMINALS.md at the repository root)
├─ static/            screen HTML · CSS · JS → static/README.md
└─ tests/             unittest (stdlib; FastAPI only for the middleware test)
```

Every folder's `__init__.py` carries a table describing what that folder does.

### Files created at runtime (all at the proxy root; paths are in `dpx/config.py`)

| File | Role |
|---|---|
| `darkpyonix.db` | SQLite: users and sessions |
| `_xmo_recent.json` | Recently opened workspace folders (contains this machine's folder paths) |
| `_dpx_machine.json` · `_dpx_hub_state.json` | Only when using the hub |

All of it is per-machine local state and is in `.gitignore`. **Do not commit it**:
`darkpyonix.db` holds password hashes and live session tokens, and `_xmo_recent.json` holds
this machine's folder paths verbatim.

### Documents

| File | Role |
|---|---|
| `docs/CONSTRAINTS.md` | **Read before touching the code.** The traps that cost the most time |
| `docs/BACKGROUND.md` | Design, what was built, what was tried and abandoned, open decisions, change history |
| `AGENTS.md` | The home screen (folders → conversations → transcripts) and the agent adapters |
| `docs/HUB.md` | Several machines on one home screen (optional) |
| `static/README.md` | When each screen file is served |

---

## Tab detach (SPEC FR-B1–B4)

Drag an editor tab out of its tab strip and drop it where VS Code does not take it (past
48 px from the strip, or outside the window) and the tab moves to a new IDE window on the
same workspace, opened at the cursor. `static/detach.js` does the webview side; the native
side is the Rust crate `crates/bridge/` (`ember-bridge`) in this repository.

**Never both.** detach.js never cancels or synthesises drag events. It decides at `dragend`,
and only when `dataTransfer.dropEffect === 'none'`: every VS Code drop target (the tab strip
= reorder, the editor area = split, terminal, explorer, chat) sets a different effect. Also
left to VS Code: Alt-drags, multi-selected tabs, and editors without a resource (Settings,
untitled). Dirty tabs are not detached (a toast asks to save first), because the new window
reads the file from disk.

VS Code has its own drag-a-tab-out-of-the-window feature
(`workbench.editor.dragToOpenWindow`, default on; in the browser it is a `window.open`
popup holding a floating editor part). To avoid two windows per gesture, the proxy sets that
setting's **default** to `false` through `configurationDefaults` in serve-web's
`vscode-workbench-web-configuration` meta (`dpx/vscode/html_rewrite.py`). If a user setting
turns it back on, detach.js notices (VS Code then withholds `text/plain` from the drag data)
and leaves drops outside the window to VS Code.

**State and how reliable it is** (read from the VS Code 1.138 workbench source, not yet
measured in a browser):

| Field | Source | Reliability |
|---|---|---|
| `fileUri` | drag data `ResourceURLs` (JSON array of URI strings) | Reliable for every file-backed editor (any tab, visible or not) |
| `cursor`, `selection` | drag data `CodeEditors[0].options.viewState.cursorState[0]` (`selectionStart`, `position`) | Reliable when the dragged editor is visible in its group or has saved view state; `null` otherwise. Diff editors: modified side |
| `scroll` | `CodeEditors[0].options.viewState.viewState.scrollTop/scrollLeft` | Same as cursor |
| fallback `fileUri` | `.monaco-editor[data-uri]` in the tab's group | Visible editor only |
| fallback `cursor` | `#status.editor.selection` text ("Ln 42, Col 7", localised; digits parsed) | Active editor only; no selection range |
| fallback `scroll` | `.lines-content` inline `top` (= −scrollTop) | Visible editor only |

Positions on the bridge are 0-based (`line`, `column`).

**Transport**: one function, `send()` in detach.js:

1. Native shell: `window.webkit.messageHandlers.emberBridge.postMessage(json)` (WKWebView) or
   `window.chrome.webview.postMessage(json)` (WebView2), looked up on the workbench iframe,
   then the wrapper (`frame.html`), then the top window. The payload is a JSON **string**.
   Native → webview: `window.__emberBridge.receive(json)` (or WebView2 `PostWebMessageAsString`);
   it dispatches an `ember-bridge` DOM event, and a version mismatch is logged, not dropped.
2. Browser fallback: `window.open` of the same workspace URL with VS Code Web's own `payload`
   query, then the source tab is closed:

   ```
   /?folder=<encodeURIComponent(folder)>&payload=<encodeURIComponent(
       [["openFile","<fileUri>:<line+1>:<column+1>"],["gotoLineMode","true"]])>
   ```

   `payload` is parsed by `WorkspaceProvider` in `src/vs/code/browser/workbench/workbench.ts`;
   `openFile`/`gotoLineMode` are consumed by `filesToOpenOrCreate` in
   `src/vs/workbench/services/environment/browser/environmentService.ts`, which splits the
   `:line:column` suffix off the URI path. When files arrive this way the workspace's previous
   editors are not restored (`layout.ts`), so the new window shows just the detached file.
   Only the cursor survives this route; the selection range and scroll do not. `frame.html`
   forwards every query parameter to its iframe, so `payload` reaches the workbench.
   If the popup is blocked (`dragend` is not an activation-triggering event), a toast offers
   an "Open" button.

The URL form is checked against `tests/vectors/bridge/detach_vectors.json` by both the JS tests
and `ember-bridge`.

Tests (no npm install, no browser):

```sh
node web/proxy/tests/js/detach.test.js        # drag state machine, extraction, message, URL, transport
cd web/proxy && python3 -m unittest           # includes tests/test_inject.py (detach.js is injected)
```

Not covered yet: touch (VS Code's tab drag is HTML5 drag-and-drop, which phones and most
tablets do not produce), and an end-to-end browser check.

---

## Further reading

Read **[docs/CONSTRAINTS.md](docs/CONSTRAINTS.md)** before touching the code. In particular:
VS Code ignores container size changes and lays out against `window.innerWidth` only, and it
reverts offsets applied to its parts on the next layout pass. That is where the time goes.
