"""DarkPyonix — the reverse proxy that stands in front of VS Code Web (`code serve-web`).

This file does **assembly and routing only**. The modules below do the actual work.

    browser ──▶ :8888  DarkPyonix (here)  ──▶ :9092   code serve-web
                       home · login · frame wrapper     stock VS Code Web
                       CSS/JS injection · WS relay

| Module                   | Role                                                |
|--------------------------|-----------------------------------------------------|
| `dpx/config.py`          | Env vars, paths, constants                          |
| `dpx/assets.py`          | Serving static/ files (all HTML, CSS and JS is there) |
| `dpx/auth/`              | The login app (`/auth`) + the session gate           |
| `dpx/vscode/`            | Upstream relay, overlay injection, the extension gate |
| `dpx/home/`              | The home screen API + the recent workspace record    |
| `dpx/agents/`            | Agent conversation adapters (Claude Code, Codex)     |
| `dpx/hub/`               | The hub showing several machines on one home (optional) |

Screen files are described in static/README.md; the full map is the "Files" section of
README.md.
"""
import logging
import sys
from urllib.parse import quote

# On 3.9 and below the dpx imports further down blow up with a SyntaxError because of
# `str | None`. That message does not explain the cause, so stop here first and say what
# the problem actually is.
if sys.version_info < (3, 10):
    raise SystemExit(
        f"DarkPyonix requires Python 3.10 or newer (this is {sys.version.split()[0]}).\n"
        "Recreate the venv with a newer version:  python -m venv .venv"
    )

from fastapi import FastAPI, Request, Response
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse, PlainTextResponse, RedirectResponse
from starlette.websockets import WebSocket

from dpx import assets
from dpx.auth.api import SESSION_COOKIE, auth_app, init_db, user_count, validate_session
from dpx.auth.gate import is_public_path
from dpx.home import api as home_api
from dpx.home import workspaces
from dpx.vscode import extension, inject, proxy

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger("proxy")

app = FastAPI()

app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)

# Auth: auth_api is a standalone FastAPI app, but it is mounted on the same origin (/auth)
# so that the session cookie reaches the proxy (/code and friends) unchanged. The gate calls
# validate_session() directly. (It can be split into its own process later — see the design
# note at the top of auth/api.py.)
init_db()
app.mount("/auth", auth_app)

_NO_USERS_HELP = """There are no accounts — nobody can log in and no workspace will open.
  To create the first account, set these environment variables and start again:
    $env:DPX_USERNAME="<user>"; $env:DPX_PASSWORD="<password>"
  A default account (admin or similar) is deliberately not created: distributing one would
  start everybody on the same credentials. How accounts should be issued is still undecided
  (docs/BACKGROUND.md §7-2)."""

if user_count() == 0:
    # Nothing can be opened in this state, so it must not pass silently.
    logger.warning("%s", _NO_USERS_HELP)

# Internal JSON APIs
app.include_router(home_api.router)      # /__workspaces, /__agents/*
app.include_router(extension.router)     # /__ext/*  (the companion extension heartbeat)


@app.on_event("startup")
async def _start_hub_connector():
    """If DPX_HUB_URL is set, run the hub connector inside this process too.

    To run it as a separate process instead, skip this hook and run
    `python -m dpx.hub.connector`.
    """
    try:
        from dpx.hub import connector
        if connector.start_background():
            logger.info("hub connector started -> %s", connector.HUB_URL)
    except Exception as exc:
        logger.info("could not start the hub connector: %s", exc)


# =============================================================================
# Our screens and assets (all read from static/)
# =============================================================================
@app.get("/__overlay.css")
async def overlay_css():
    return assets.stylesheet("overlay.css")


@app.get("/__overlay.js")
async def overlay_js():
    return assets.script("overlay.js")


@app.get("/__kb.js")
async def webview_kb_js():
    """The webview-frame-only keyboard policy. Never injected into the workbench, which
    already has the full policy."""
    return assets.script("webview-kb.js")


@app.get("/favicon.ico")
async def favicon():
    # Passing this to the proxy makes serve-web answer 404 (or 502 when upstream is down),
    # and the browser then asks again on every page. End it with a 204.
    return Response(status_code=204)


# Must be registered **before** the catch-all below. Otherwise it is shadowed, goes upstream
# and 404s.
@app.get("/healthz")
async def healthz():
    return PlainTextResponse("ok")


def _serve_our_page(request: Request, path: str, xmo: str) -> Response | None:
    """The response if this request is one of our screens, else None (→ upstream).

    A workspace (`/?folder=…`) **always gets the frame wrapper**, regardless of device type.
    The wrapper turns its own chrome (the bar) on and off by viewport width, so on a wide
    window the iframe fills 100% and VS Code's native screen shows through unchanged, and
    narrowing it brings our bar up immediately, with no reload.

    Why the wrapper is needed: the 48px activity bar column the VS Code grid reserves cannot
    be removed with CSS. Only the wrapper can slide the iframe left by that much and push the
    dead column off-screen.

    The one exception is `xmo=embed`, which is not a development switch but **the internal
    value the wrapper attaches when loading its own iframe**. Without filtering it here, the
    iframe would become a wrapper too, nesting forever.
    """
    if request.method != "GET" or "text/html" not in request.headers.get("accept", ""):
        return None
    if path == "/login":
        return assets.page("workspace_login.html")
    if path == "/":
        if "folder" not in request.query_params:
            return assets.page("home.html")          # home launcher (folders → convos → transcript)
        if xmo != "embed":
            workspaces.record(request.query_params.get("folder"))
            return assets.page("frame.html")         # workspace = parent page + iframe
    return None


# =============================================================================
# The path a single request takes — in order, top to bottom
#   1. Internal endpoints (/healthz, /__ext/*) pass straight through
#   2. The auth gate: anything not public needs a valid session
#   3. Our screens (home, login, the frame wrapper) are served from static/ and end here
#   4. The workbench main CSS → with the overlay CSS appended
#   5. Webview host frames → with the keyboard policy JS added
#   6. The top-level workbench document (`/`) → with viewport and overlay added
#   7. Everything else → relayed upstream untouched
# =============================================================================
@app.middleware("http")
async def route_request(request: Request, call_next):
    path = request.url.path
    # Entry log: uvicorn only prints **finished** responses, so a request that stalls inside
    # the proxy is invisible without this line (which was needed to track down device freezes).
    logger.info(">> %s %s", request.method, path[:110])

    # 1. Internal endpoints are neither intercepted nor proxied.
    if path == "/healthz" or path.startswith("/__ext/"):
        return await call_next(request)

    # 2. The auth gate — every request bound for serve-web (editor + assets) needs a valid
    #    session. This replaces VS Code's connection token.
    if not is_public_path(path):
        if not validate_session(request.cookies.get(SESSION_COOKIE)):
            if request.method == "GET" and "text/html" in request.headers.get("accept", ""):
                # A top-level navigation → send it to the login page, preserving the destination
                return RedirectResponse(f"/login?next={quote(str(request.url))}", status_code=303)
            return JSONResponse({"error": "unauthorized"}, status_code=401)

    xmo = request.query_params.get("xmo", "").lower()

    # 3. Our screens
    page = _serve_our_page(request, path, xmo)
    if page is not None:
        return page

    # 4. The workbench main CSS
    if inject.is_main_css(path):
        return await inject.main_css(request, path)

    accept = request.headers.get("accept", "")
    if request.method == "GET" and "text/html" in accept:
        folder = request.query_params.get("folder")
        if folder:
            workspaces.record(folder)

        # 5. Webview host frames. Extension panels (agent chat) are drawn inside these, and the
        #    workbench's keyboard policy cannot see that document — which is why the soft
        #    keyboard rose every time a new conversation opened. Give those frames a small
        #    policy of their own.
        if inject.is_webview_host(path):
            return await inject.webview_kb(request, path)

        # 6. Inject the overlay into the top-level workbench document only. VS Code also serves
        #    internal HTML documents (webWorkerExtensionHostIframe.html and friends); putting
        #    our script in one of those ran it inside the extension host and froze Android
        #    completely.
        if path != "/":
            return await proxy.relay(request, proxy.upstream_url(path))
        return await inject.workbench_html(request, path)

    # 7. Everything else falls through to the catch-all route below.
    return await call_next(request)


# =============================================================================
# The last gate before upstream — everything nobody above claimed
# =============================================================================
@app.api_route("/{full_path:path}",
               methods=["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD"])
async def catch_all(request: Request, full_path: str):
    return await proxy.relay(request, proxy.upstream_url(f"/{full_path}"))


@app.websocket("/{full_path:path}")
async def websocket_proxy(websocket: WebSocket, full_path: str):
    """Relays the WebSockets VS Code Web uses (the extension host and so on). The actual pump
    is proxy.relay_websocket."""
    logger.info(">> WS OPEN /%s", full_path[:100])
    # Every WS is a workspace resource → refuse the upgrade without a valid session.
    if not validate_session(websocket.cookies.get(SESSION_COOKIE)):
        logger.info(">> WS DENIED (no session) /%s", full_path[:80])
        await websocket.close(code=1008)
        return
    try:
        await proxy.relay_websocket(websocket, full_path)
    finally:
        logger.info("<< WS CLOSE /%s", full_path[:100])
