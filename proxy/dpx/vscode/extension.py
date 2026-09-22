"""The companion VS Code extension's gate + `/__ext/*`.

DarkPyonix only alters the screen while the companion extension (app/extension) is
installed and active. The extension host posts to `/__ext/ping` roughly every 10 seconds;
if no heartbeat arrives for EXT_TTL_SECONDS (disabled, uninstalled, or a host crash), the
proxy falls back to being a transparent pipe passing stock VS Code Web through.

`/__ext/*` is public, because the extension calls it without logging in (see
`auth/gate.py`).
"""
import logging
import time

from fastapi import APIRouter, Request

from dpx.config import EXT_TTL_SECONDS

logger = logging.getLogger("proxy")

router = APIRouter()

EXT_STATE = {"last_ping": 0.0, "enabled": False}


def active() -> bool:
    """Is the extension alive right now — that is, may we alter the screen?"""
    return EXT_STATE["enabled"] and (time.monotonic() - EXT_STATE["last_ping"]) < EXT_TTL_SECONDS


def _record_ping(enabled: bool) -> bool:
    """Records a heartbeat. True if the gate state changed."""
    was_active = active()
    EXT_STATE["enabled"] = enabled
    EXT_STATE["last_ping"] = time.monotonic()
    return active() != was_active


def _ping_age() -> float | None:
    if not EXT_STATE["last_ping"]:
        return None
    return round(time.monotonic() - EXT_STATE["last_ping"], 1)


@router.post("/__ext/ping")
async def ext_ping(request: Request):
    try:
        data = await request.json()
    except Exception:
        data = {}
    if _record_ping(bool((data or {}).get("enabled", True))):
        logger.info("DarkPyonix extension gate -> %s", "ON" if active() else "OFF")
    return {"ok": True, "active": active(), "ttl": EXT_TTL_SECONDS}


@router.get("/__ext/state")
async def ext_state():
    return {"active": active(), "enabled": EXT_STATE["enabled"], "ping_age_seconds": _ping_age()}
