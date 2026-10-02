# static/ — pages and injected assets

These files used to live as Python strings inside `main.py` (`CUSTOM_OVERLAY_CSS`,
`OVERLAY_BOOT_JS`, `FRAME_PAGE_HTML`, …). No editor could highlight them and `main.py`
kept growing, so they were pulled out here. On the Python side, `dpx/assets.py` only
reads and serves them.

## Files the proxy (`main.py`) uses

| File | When it is served | Served by |
|---|---|---|
| `home.html` | `GET /` (no folder) — the home launcher: folders → conversations → transcript | `main._serve_our_page` |
| `frame.html` | `GET /?folder=…` — the workspace wrapper (parent page + VS Code iframe) | `main._serve_our_page` |
| `workspace_login.html` | `GET /login` — the workspace login page | `main._serve_our_page` |
| `upstream_down.html` | The 502 explainer shown when `code serve-web` is down | `dpx.vscode.proxy.upstream_down` |
| `overlay.css` | `GET /__overlay.css`, and appended after the workbench's main CSS | `dpx.vscode.inject.main_css` |
| `overlay.js` | `GET /__overlay.js` — injected into the workbench top-level document only | `dpx.vscode.inject.workbench_html` |
| `webview-kb.js` | `GET /__kb.js` — injected into VS Code webview host frames only | `dpx.vscode.inject.webview_kb` |

## Files the hub (`dpx/hub/server.py`) uses

| File | Description |
|---|---|
| `hub_home.html` | The hub home, showing several machines on one screen |
| `login.html` | The hub login page (a different page from the proxy's `workspace_login.html`) |

## Editing these

`dpx/assets.py` caches each file for the lifetime of the process. `uvicorn --reload`
only restarts on `*.py` changes, so to see HTML/CSS/JS edits immediately, start with the
cache disabled:

```powershell
$env:DPX_NO_ASSET_CACHE="1"
```

Browser caching takes care of itself: `ASSET_VERSION` (the process start time) is appended
as a query parameter.

## Warning

Never inject `overlay.js` anywhere but the workbench **top-level document**. Injecting it
into an internal HTML document VS Code serves (`webWorkerExtensionHostIframe.html` and
friends) ran the script inside the extension host and froze Android browsers completely.
Webview frames get only the much smaller `webview-kb.js`.
