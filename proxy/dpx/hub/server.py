"""The DarkPyonix hub — a relay server gathering several machines onto one home screen. (Optional.)

*This file is only needed when you use more than one machine.* With just one, main.py's home
screen already shows folders → conversations → transcripts, so there is no reason to run a hub.

Shape
-----
    machine (connector) ──outbound WS──▶ [hub] ◀──HTTPS── phone/desktop browser (home screen)

The hub holds the snapshots each machine pushes (workspaces plus recent conversation
summaries) in memory and hands them straight to the home screen. That is why the first screen
renders immediately without waking any machine, and only large things, such as a full
transcript, trigger an RPC to that machine on demand.

Even with a machine powered off, its last snapshot stays on disk, so the card does not
disappear (it is shown offline, with the last-seen time).

Authentication
--------------
* Browsers: the auth_api session cookie (the same login as main.py) — required on all of /api/*
* Connectors: the DPX_HUB_TOKEN shared token

Running it
----------
    set DPX_HUB_TOKEN=<any long string>
    uvicorn hub:app --host 0.0.0.0 --port 8900
"""
from __future__ import annotations

import asyncio
import hmac
import json
import logging
import os
import time
from urllib.parse import quote

from fastapi import FastAPI, Request, WebSocket, WebSocketDisconnect
from fastapi.responses import JSONResponse, RedirectResponse, Response
from fastapi.staticfiles import StaticFiles

from dpx.auth.api import auth_app, init_db, validate_session, SESSION_COOKIE
from dpx.config import HUB_STATE_FILE, STATIC_DIR

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger("hub")

HUB_TOKEN = os.environ.get("DPX_HUB_TOKEN", "")
OFFLINE_AFTER = float(os.environ.get("DPX_OFFLINE_AFTER", "75"))   # seconds
RPC_TIMEOUT = float(os.environ.get("DPX_RPC_TIMEOUT", "25"))
STATE_FILE = HUB_STATE_FILE   # the repository root (dpx/config.py)


# --- The machine registry ----------------------------------------------------
class Machine:
    """One machine. The object survives a dropped connection and keeps serving the last snapshot."""

    def __init__(self, info: dict):
        self.info = info
        self.ws: WebSocket | None = None
        self.snapshot: dict = {}
        self.last_seen: float = 0.0
        self.connected_at: float = 0.0
        self._seq = 0
        self._pending: dict[int, asyncio.Future] = {}

    @property
    def online(self) -> bool:
        return self.ws is not None and (time.time() - self.last_seen) < OFFLINE_AFTER

    def to_dict(self) -> dict:
        snap = self.snapshot or {}
        return {
            "id": self.info.get("id", ""),
            "name": self.info.get("name", "이름 없는 PC"),
            "os": self.info.get("os", ""),
            "public_url": self.info.get("public_url", ""),
            "agents": self.info.get("agents", []),
            "online": self.online,
            "last_seen": self.last_seen,
            "workspace_count": len(snap.get("workspaces", [])),
            "conversation_count": len(snap.get("conversations", [])),
        }

    async def rpc(self, method: str, params: dict | None = None) -> dict:
        if not self.online or self.ws is None:
            raise RuntimeError("offline")
        self._seq += 1
        rid = self._seq
        fut: asyncio.Future = asyncio.get_running_loop().create_future()
        self._pending[rid] = fut
        try:
            await self.ws.send_text(json.dumps({"t": "rpc", "id": rid, "method": method,
                                                "params": params or {}}))
            return await asyncio.wait_for(fut, RPC_TIMEOUT)
        finally:
            self._pending.pop(rid, None)

    def resolve(self, rid: int, ok: bool, payload) -> None:
        fut = self._pending.get(rid)
        if not fut or fut.done():
            return
        if ok:
            fut.set_result(payload or {})
        else:
            fut.set_exception(RuntimeError(str(payload)))


MACHINES: dict[str, Machine] = {}


def save_state() -> None:
    """Persists the last snapshot to disk, so restarting the hub does not empty the cards."""
    try:
        data = {mid: {"info": m.info, "snapshot": m.snapshot, "last_seen": m.last_seen}
                for mid, m in MACHINES.items()}
        STATE_FILE.write_text(json.dumps(data, ensure_ascii=False), encoding="utf-8")
    except Exception:
        pass


def load_state() -> None:
    try:
        data = json.loads(STATE_FILE.read_text(encoding="utf-8"))
    except Exception:
        return
    for mid, entry in (data or {}).items():
        m = Machine(entry.get("info") or {"id": mid})
        m.snapshot = entry.get("snapshot") or {}
        m.last_seen = float(entry.get("last_seen") or 0)
        MACHINES[mid] = m


# --- The app -----------------------------------------------------------------
app = FastAPI(title="DarkPyonix Hub")
init_db()
app.mount("/auth", auth_app)
if STATIC_DIR.is_dir():
    app.mount("/static", StaticFiles(directory=str(STATIC_DIR)), name="static")


@app.on_event("startup")
async def _startup():
    load_state()
    if not HUB_TOKEN:
        logger.warning("DPX_HUB_TOKEN is empty; rejecting every connector.")
    logger.info("hub started, %d machine(s) registered", len(MACHINES))


def _authed(request: Request) -> bool:
    return validate_session(request.cookies.get(SESSION_COOKIE))


def _need_login() -> JSONResponse:
    return JSONResponse({"ok": False, "error": "로그인이 필요합니다"}, status_code=401)


def _machine(mid: str) -> Machine | None:
    return MACHINES.get(mid)


# --- The connector WebSocket -------------------------------------------------
@app.websocket("/hub/connect")
async def hub_connect(ws: WebSocket):
    token = ws.query_params.get("token", "")
    if not HUB_TOKEN or not hmac.compare_digest(token, HUB_TOKEN):
        await ws.close(code=1008)
        logger.info("connector authentication failed")
        return
    await ws.accept()

    machine: Machine | None = None
    try:
        raw = await asyncio.wait_for(ws.receive_text(), 20)
        hello = json.loads(raw)
        info = hello.get("machine") or {}
        mid = str(info.get("id") or "").strip()
        if not mid:
            await ws.close(code=1008)
            return
        machine = MACHINES.get(mid) or Machine(info)
        machine.info = info
        machine.ws = ws
        machine.last_seen = machine.connected_at = time.time()
        if hello.get("snapshot"):
            machine.snapshot = hello["snapshot"]
        MACHINES[mid] = machine
        save_state()
        logger.info("connector attached: %s (%s)", info.get("name"), mid)

        while True:
            msg = json.loads(await ws.receive_text())
            machine.last_seen = time.time()
            kind = msg.get("t")
            if kind == "snapshot":
                machine.snapshot = msg.get("snapshot") or {}
                save_state()
            elif kind == "rpc_result":
                machine.resolve(int(msg.get("id") or 0), bool(msg.get("ok")),
                                msg.get("result") if msg.get("ok") else msg.get("error"))
            elif kind == "hello":
                machine.info = msg.get("machine") or machine.info
    except (WebSocketDisconnect, asyncio.TimeoutError):
        pass
    except Exception as exc:
        logger.info("connector error: %s", exc)
    finally:
        if machine is not None:
            machine.ws = None
            for fut in list(machine._pending.values()):
                if not fut.done():
                    fut.set_exception(RuntimeError("연결이 끊겼습니다"))
            machine._pending.clear()
            save_state()
            logger.info("connector detached: %s", machine.info.get("name"))


# --- REST for the home screen ------------------------------------------------
@app.get("/api/machines")
async def api_machines(request: Request):
    if not _authed(request):
        return _need_login()
    rows = sorted((m.to_dict() for m in MACHINES.values()),
                  key=lambda d: (d["online"], d["last_seen"]), reverse=True)
    return {"ok": True, "machines": rows}


@app.get("/api/tree")
async def api_tree(request: Request):
    """One call for the first home screen: every machine, its workspaces and conversation summaries, from the cached snapshots."""
    if not _authed(request):
        return _need_login()
    out = []
    for m in sorted(MACHINES.values(), key=lambda x: (x.online, x.last_seen), reverse=True):
        snap = m.snapshot or {}
        convs = snap.get("conversations", [])
        by_folder: dict[str, list] = {}
        for c in convs:
            by_folder.setdefault(str(c.get("folder", "")).replace("\\", "/").rstrip("/").lower(), []).append(c)
        workspaces = []
        for ws in snap.get("workspaces", []):
            key = str(ws.get("folder", "")).replace("\\", "/").rstrip("/").lower()
            workspaces.append({**ws, "recent": by_folder.get(key, [])[:5]})
        row = m.to_dict()
        row["workspaces"] = workspaces
        out.append(row)
    return {"ok": True, "machines": out, "ts": time.time()}


@app.get("/api/machines/{mid}/conversations")
async def api_conversations(request: Request, mid: str, folder: str = "", agent: str = "",
                            fresh: int = 0):
    if not _authed(request):
        return _need_login()
    m = _machine(mid)
    if not m:
        return JSONResponse({"ok": False, "error": "알 수 없는 PC"}, status_code=404)
    if fresh and m.online:
        try:
            res = await m.rpc("conversations", {"folder": folder, "agent": agent})
            return {"ok": True, "conversations": res.get("conversations", []), "fresh": True}
        except Exception as exc:
            logger.info("conversations RPC failed: %s", exc)
    convs = (m.snapshot or {}).get("conversations", [])
    if folder:
        want = folder.replace("\\", "/").rstrip("/").lower()
        convs = [c for c in convs if str(c.get("folder", "")).replace("\\", "/").rstrip("/").lower() == want]
    if agent:
        convs = [c for c in convs if c.get("agent") == agent]
    return {"ok": True, "conversations": convs, "fresh": False, "online": m.online}


@app.get("/api/machines/{mid}/transcript")
async def api_transcript(request: Request, mid: str, agent: str, cid: str, limit: int = 300):
    if not _authed(request):
        return _need_login()
    m = _machine(mid)
    if not m:
        return JSONResponse({"ok": False, "error": "알 수 없는 PC"}, status_code=404)
    if not m.online:
        return JSONResponse({"ok": False, "error": "이 PC 가 오프라인입니다. 전체 기록은 켜져 있을 때만 읽을 수 있어요."},
                            status_code=503)
    try:
        res = await m.rpc("transcript", {"agent": agent, "cid": cid, "limit": limit})
        return {"ok": True, "messages": res.get("messages", [])}
    except Exception as exc:
        return JSONResponse({"ok": False, "error": f"기록을 읽지 못했습니다: {exc}"}, status_code=502)


@app.get("/api/machines/{mid}/open")
async def api_open(request: Request, mid: str, folder: str, agent: str = "", cid: str = ""):
    """The URL and resume command the "Resume in VS Code" button uses."""
    if not _authed(request):
        return _need_login()
    m = _machine(mid)
    if not m:
        return JSONResponse({"ok": False, "error": "알 수 없는 PC"}, status_code=404)
    base = (m.info.get("public_url") or "").rstrip("/")
    url = ""
    if base:
        url = f"{base}/?folder={quote(folder)}"
        if agent and cid:
            url += f"&agent={quote(agent)}&cid={quote(cid)}"
    resume = None
    if m.online and agent and cid:
        try:
            res = await m.rpc("resume", {"agent": agent, "cid": cid})
            resume = res.get("resume")
        except Exception:
            resume = None
    return {"ok": True, "url": url, "resume": resume, "online": m.online,
            "reason": "" if base else "이 PC 에 DPX_PUBLIC_URL 이 설정되어 있지 않아 바로 열 수 없습니다."}


@app.get("/api/machines/{mid}/refresh")
async def api_refresh(request: Request, mid: str):
    if not _authed(request):
        return _need_login()
    m = _machine(mid)
    if not m or not m.online:
        return JSONResponse({"ok": False, "error": "오프라인"}, status_code=503)
    try:
        snap = await m.rpc("snapshot", {})
        m.snapshot = snap
        save_state()
        return {"ok": True}
    except Exception as exc:
        return JSONResponse({"ok": False, "error": str(exc)}, status_code=502)


@app.get("/healthz")
async def healthz():
    online = sum(1 for m in MACHINES.values() if m.online)
    return {"ok": True, "machines": len(MACHINES), "online": online}


@app.get("/favicon.ico")
async def favicon():
    # Without this the browser logs a 404 on every page. End it quietly with a 204.
    return Response(status_code=204)


# --- Pages -------------------------------------------------------------------
def _page(name: str) -> Response:
    path = STATIC_DIR / name
    if not path.is_file():
        return Response(content=f"{name} 이 없습니다.", status_code=500, media_type="text/plain")
    return Response(content=path.read_text(encoding="utf-8"), media_type="text/html",
                    headers={"Cache-Control": "no-cache, no-store, must-revalidate"})


@app.get("/login")
async def login_page():
    return _page("login.html")


@app.get("/")
async def home(request: Request):
    if not _authed(request):
        return RedirectResponse("/login", status_code=303)
    return _page("hub_home.html")
