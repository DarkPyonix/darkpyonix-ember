"""The DarkPyonix connector — runs on each machine and reports itself to the hub.

Why outbound
------------
Instead of setting up port forwarding and a certificate on every home, school and office
machine, **the machine dials out to the hub** over a WebSocket. No router configuration is
needed, and the phone only has to know one address: the hub.

What it sends
-------------
1) hello    - machine info (name, OS, public URL, available agents)
2) snapshot - the workspace list + recent conversation summaries (the hub caches these so the
              first home screen renders immediately)
3) rpc replies - a full transcript, read and sent only when the hub asks for it

Running it
----------
    set DPX_HUB_URL=wss://hub.example.com/hub/connect
    set DPX_HUB_TOKEN=<the same token as the hub>
    set DPX_PUBLIC_URL=https://my-pc.example.com:8888   (this machine's DarkPyonix address)
    python -m dpx.hub.connector

To run it inside the same process as main.py, use start_background().
"""
from __future__ import annotations

import asyncio
import json
import logging
import os
import platform
import socket
import time
import uuid

import websockets

from dpx import agents
from dpx.config import MACHINE_ID_FILE

logger = logging.getLogger("connector")

HUB_URL = os.environ.get("DPX_HUB_URL", "")
HUB_TOKEN = os.environ.get("DPX_HUB_TOKEN", "")
PUBLIC_URL = os.environ.get("DPX_PUBLIC_URL", "").rstrip("/")
MACHINE_NAME = os.environ.get("DPX_MACHINE_NAME") or socket.gethostname()
SNAPSHOT_INTERVAL = float(os.environ.get("DPX_SNAPSHOT_INTERVAL", "20"))
SNAPSHOT_CONVERSATIONS = int(os.environ.get("DPX_SNAPSHOT_CONVERSATIONS", "40"))
ID_FILE = MACHINE_ID_FILE     # the repository root (dpx/config.py)


def machine_id() -> str:
    """A stable identifier for this machine, persisted to a file so it stays the same card across restarts."""
    env = os.environ.get("DPX_MACHINE_ID")
    if env:
        return env
    try:
        data = json.loads(ID_FILE.read_text(encoding="utf-8"))
        if data.get("id"):
            return str(data["id"])
    except Exception:
        pass
    mid = uuid.uuid4().hex[:12]
    try:
        ID_FILE.write_text(json.dumps({"id": mid, "name": MACHINE_NAME}), encoding="utf-8")
    except Exception:
        pass
    return mid


def machine_info() -> dict:
    return {
        "id": machine_id(),
        "name": MACHINE_NAME,
        "os": f"{platform.system()} {platform.release()}".strip(),
        "public_url": PUBLIC_URL,
        "agents": [{"id": a.id, "name": a.name} for a in agents.active_adapters()],
        "version": 1,
    }


# --- Local lookups (all blocking I/O, so wrapped in to_thread) ----------------
def _snapshot() -> dict:
    # There is no reason to send source (a local file path) to the phone; it only inflates the payload.
    convs = agents.conversations(limit=SNAPSHOT_CONVERSATIONS)
    for c in convs:
        c.pop("source", None)
    return {
        "workspaces": agents.workspaces(),
        "conversations": convs,
        "agents": [{"id": a.id, "name": a.name} for a in agents.active_adapters()],
        "ts": time.time(),
    }


RPC_METHODS = {
    "ping": lambda p: {"pong": time.time()},
    "workspaces": lambda p: {"workspaces": agents.workspaces()},
    "conversations": lambda p: {"conversations": agents.conversations(
        folder=p.get("folder") or None, agent=p.get("agent") or None,
        limit=int(p.get("limit") or 200))},
    "transcript": lambda p: {"messages": agents.transcript(
        p.get("agent") or "", p.get("cid") or "", int(p.get("limit") or 300))},
    "resume": lambda p: {"resume": agents.resume(p.get("agent") or "", p.get("cid") or "")},
    "snapshot": lambda p: _snapshot(),
}


async def _dispatch(method: str, params: dict) -> dict:
    fn = RPC_METHODS.get(method)
    if not fn:
        raise ValueError(f"unknown method: {method}")
    return await asyncio.to_thread(fn, params or {})


# --- Connecting to the hub ---------------------------------------------------
async def _serve(ws) -> None:
    """The lifetime after one successful connection: the snapshot push task plus the receive loop."""
    await ws.send(json.dumps({"t": "hello", "machine": machine_info(),
                              "snapshot": await asyncio.to_thread(_snapshot)}))

    async def push_snapshots():
        last = ""
        while True:
            await asyncio.sleep(SNAPSHOT_INTERVAL)
            snap = await asyncio.to_thread(_snapshot)
            # Nothing changed → send a short heartbeat instead of a snapshot, saving phone data and battery.
            sig = json.dumps([[c.get("id"), c.get("updated")] for c in snap.get("conversations", [])])
            if sig == last:
                await ws.send(json.dumps({"t": "beat"}))
                continue
            last = sig
            await ws.send(json.dumps({"t": "snapshot", "snapshot": snap}))

    pusher = asyncio.create_task(push_snapshots())
    try:
        async for raw in ws:
            try:
                msg = json.loads(raw)
            except Exception:
                continue
            if msg.get("t") != "rpc":
                continue
            rid, method = msg.get("id"), msg.get("method") or ""
            try:
                result = await _dispatch(method, msg.get("params") or {})
                await ws.send(json.dumps({"t": "rpc_result", "id": rid, "ok": True, "result": result}))
            except Exception as exc:
                logger.info("rpc %s failed: %s", method, exc)
                await ws.send(json.dumps({"t": "rpc_result", "id": rid, "ok": False, "error": str(exc)}))
    finally:
        pusher.cancel()


async def run_forever(hub_url: str = "", token: str = "") -> None:
    hub_url = hub_url or HUB_URL
    token = token or HUB_TOKEN
    if not hub_url:
        logger.warning("DPX_HUB_URL is not set; not starting the connector.")
        return
    url = hub_url + ("&" if "?" in hub_url else "?") + "token=" + token
    delay = 2.0
    while True:
        try:
            logger.info("connecting to hub: %s", hub_url)
            async with websockets.connect(url, ping_interval=20, ping_timeout=20,
                                          max_size=8 * 1024 * 1024, open_timeout=20) as ws:
                logger.info("connected to hub (machine=%s)", machine_id())
                delay = 2.0
                await _serve(ws)
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            logger.info("hub connection dropped or failed: %s", exc)
        await asyncio.sleep(delay)
        delay = min(delay * 1.7, 60.0)      # exponential backoff, so a dead hub does not flood the log


def start_background() -> asyncio.Task | None:
    """Attaches the connector to an existing event loop, such as main.py's."""
    if not HUB_URL:
        return None
    return asyncio.create_task(run_forever())


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    try:
        asyncio.run(run_forever())
    except KeyboardInterrupt:
        pass
