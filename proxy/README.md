# DarkPyonix — the VS Code Web wrapping layer

A reverse proxy that makes VS Code Web (`code serve-web`) usable on tablets and phones.
It serves the home launcher and the login page itself, and wraps a workspace in an iframe
so that a parent page owns the navigation bar. That bar is turned on and off by window
width, so on a wide screen you see stock VS Code, untouched.

In Ember's vocabulary this is the wrapping layer of `FR-W2`: CSS/DOM overrides layered on
an official, unmodified VS Code Web build. No upstream source is patched.

---

## ⚠️ There are two processes

`code serve-web` is **stock VS Code Web and nothing else**.
**Every screen we built — the bar, home, login, the wrapper — lives in the proxy (:8888).**
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

# 1) Virtualenv — Python 3.10 or newer is required (check with: python -V)
python -m venv .venv
.\.venv\Scripts\python.exe -m pip install -r requirements.txt

# 2) The first account — without this there is no account, so nothing can be opened
$env:DPX_USERNAME="<user>"; $env:DPX_PASSWORD="<password>"
```

⚠️ **No default account is created** — both variables must be set. Login is still a thin
shell (see [docs/BACKGROUND.md](docs/BACKGROUND.md)). The variables apply exactly once, when
`darkpyonix.db` does not yet exist, so if the account already exists use
`/auth/change-password` or delete the database and start again.

That machine also needs `code serve-web` (it ships with VS Code).

⚠️ **Using this from a phone or tablet requires HTTPS, and how to obtain it is undecided** —
see the section below.

---

## Running it

Once, the first time (skip if you already did the "new machine" section):

```powershell
Set-Location <the folder you cloned into>
python -m venv .venv
.\.venv\Scripts\python.exe -m pip install -r requirements.txt
```

Every time, in two terminals:

```powershell
# 1) Upstream VS Code Web
code serve-web --host 127.0.0.1 --port 9094 --without-connection-token --accept-server-license-terms

# 2) The proxy — the port above must be passed as XMO_UPSTREAM_PORT (the default is 9092)
$env:XMO_UPSTREAM_PORT="9094"; .\.venv\Scripts\python.exe -m uvicorn main:app --host 0.0.0.0 --port 8888
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

---

## Screen flow

```
/                     home launcher — workspace cards
  └─ [Open] ──▶ /login?...        login page
        └─ POST /auth/login ──▶ /?folder=<path>   workspace
```

- Workspaces (`/?folder=...`) and the serve-web assets and WebSocket **require a valid
  session**. Requesting HTML without one returns a 303 redirect to
  `/login?next=<original address>`.
- **There is no initial account** — one is created only on first start, and only if
  `DPX_USERNAME` and `DPX_PASSWORD` are both set. Without an account, the startup log warns
  and logging in is impossible.
- ⚠️ **An account is an entry pass, not an identity** — there is no per-user file isolation,
  so whoever logs in sees the files of the OS account that started the server. Using one
  account from a laptop, a phone and a tablet at the same time is supported on purpose. See
  [docs/BACKGROUND.md](docs/BACKGROUND.md).
- The workspace screen switches on window width — wide gives stock VS Code, narrow or touch
  gives our bar. The switch happens **without a reload**.

---

## Configuration (environment variables)

| Variable | Default | Description |
|---|---|---|
| `XMO_UPSTREAM_HOST` | `127.0.0.1` | serve-web host |
| `XMO_UPSTREAM_PORT` | `9092` | serve-web port — **must be set if you started it on another port** |
| `DPX_USERNAME` | (none) | Name of the first account — **both variables are required to create it** |
| `DPX_PASSWORD` | (none) | Password of the first account. Without it, the server starts with no account |
| `DPX_NO_ASSET_CACHE` | (none) | `1` re-reads `static/` on every request — for working on the screens |
| `DPX_HOME` | user home | Where to look for agent session files (`~/.claude`, …) |
| `DPX_HUB_URL` · `DPX_HUB_TOKEN` | (none) | If set, the hub connector starts alongside → [docs/HUB.md](docs/HUB.md) |

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
  node --check static\overlay.js; node --check static\webview-kb.js
  ```
  The `<script>` inside `frame.html` is still inline, so check that one in the browser console.
- To verify layout with browser automation, install Playwright separately (it is not a runtime
  dependency, so it is not in `requirements.txt`):
  `pip install playwright && playwright install chromium`.
  ⚠️ **Headless Chromium cannot render extension webviews at all** — only geometry
  (coordinates, styles) can be verified there, and whether a webview actually appears is only
  confirmed on a real device.

---

## URL parameters

The normal path is `/?folder=<path>` and **nothing else**. Whether to wrap and whether to show
the bar are decided automatically.

The development bypass switches (`xmo=on`, `xmo=off`, `xmodebug`, `xmojs`, `xmokb`, `xmofs`)
and the legacy overlay mode have **all been removed**. There is no switch left to touch.

`xmo=embed`, `xmochrome` and `xmoorient` still exist, but they are not switches — they are
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
| `/__workspaces` | Recent workspaces the server remembers (`_xmo_recent.json`) |
| `/__agents/*` | Agent conversation API, used by the home screen → [AGENTS.md](AGENTS.md) |
| `/healthz` | Health check → `ok` |
| `/__ext/ping` · `/__ext/state` | Extension-gating receiver — **plumbing only, unused** |
| everything else | Proxied to serve-web (including WebSocket) |

---

## Files

`main.py` used to hold the HTML, CSS, JS, API and proxy all at once; it has been split up.
**To change a screen open `static/`; to change behavior open one folder under `dpx/`.**

```
proxy/
├─ main.py            app assembly + request routing ← read only this for the overall flow
├─ dpx/
│  ├─ config.py       env vars · paths · constants (every setting lives here)
│  ├─ assets.py       serving static/ files
│  ├─ auth/           authentication
│  │  ├─ gate.py        which paths are open without a session
│  │  └─ api.py         the /auth login app — users · sessions · SQLite
│  ├─ vscode/         everything on the VS Code Web side
│  │  ├─ proxy.py       upstream relay (HTTP streaming · WebSocket)
│  │  ├─ inject.py      where the overlay CSS/JS gets injected
│  │  └─ extension.py   companion-extension heartbeat gate + /__ext/*
│  ├─ home/           the back end of the home screen
│  │  ├─ api.py         /__agents/* · /__workspaces
│  │  └─ workspaces.py  the record of recently opened folders
│  ├─ agents/         conversation adapters (Claude Code · Codex) → AGENTS.md
│  └─ hub/            several machines on one home screen (optional) → docs/HUB.md
│     ├─ server.py
│     └─ connector.py
└─ static/            screen HTML · CSS · JS → static/README.md
```

Every folder's `__init__.py` carries a table describing what that folder does.

### Files created at runtime (all at the proxy root; paths are in `dpx/config.py`)

| File | Role |
|---|---|
| `darkpyonix.db` | SQLite — users and sessions |
| `_xmo_recent.json` | Recently opened workspace folders (contains this machine's folder paths) |
| `_dpx_machine.json` · `_dpx_hub_state.json` | Only when using the hub |

All of it is per-machine local state and is in `.gitignore`. **Do not commit it** —
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

## Further reading

Read **[docs/CONSTRAINTS.md](docs/CONSTRAINTS.md)** before touching the code. In particular:
VS Code ignores container size changes and lays out against `window.innerWidth` only, and it
reverts offsets applied to its parts on the next layout pass. That is where the time goes.
