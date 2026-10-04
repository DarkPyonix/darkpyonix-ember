"""The JSON API the home screen (static/home.html) uses.

| Endpoint                  | Description                             |
|---------------------------|-----------------------------------------|
| `/__workspaces`           | Recent workspaces the server remembers   |
| `/__agents/workspaces`    | Folder list + 3 recent conversations each |
| `/__agents/conversations` | One folder's conversation list           |
| `/__agents/transcript`    | One conversation's full transcript       |
| `/__agents/resume`        | The resume command (`claude --resume <id>`, etc.) |

None of these are public, so a valid session is required (the gate answers 401).
Reading session files is blocking I/O, so it is handed to to_thread to keep the event loop
free.

The extension heartbeat (`/__ext/*`) is not here; it lives in `dpx/vscode/extension.py`.
"""
import asyncio
import logging

from fastapi import APIRouter
from fastapi.responses import JSONResponse

from dpx.home import workspaces

logger = logging.getLogger("proxy")

router = APIRouter()

# The agent conversation adapters (agents/). The proxy itself must keep working even if they
# are missing or broken, so an import failure is swallowed and the home screen simply shows
# "no conversations".
try:
    from dpx import agents as agents_mod
except Exception as _agents_exc:            # pragma: no cover
    agents_mod = None
    logger.info("could not import the agents package: %s", _agents_exc)


def _no_agents(extra: dict) -> dict:
    return {"ok": True, "agents": [], **extra}


@router.get("/__workspaces")
async def list_workspaces():
    return {"workspaces": [{"path": w, "name": workspaces.folder_name(w)}
                           for w in workspaces.RECENT_WORKSPACES]}


@router.get("/__agents/workspaces")
async def agents_workspaces():
    if agents_mod is None:
        return _no_agents({"workspaces": []})

    def build():
        spaces = agents_mod.workspaces()
        convs = agents_mod.conversations(limit=300)
        by_folder: dict[str, list] = {}
        for c in convs:
            c.pop("source", None)
            by_folder.setdefault(workspaces.folder_key(c.get("folder", "")), []).append(c)
        for w in spaces:
            w["recent"] = by_folder.get(workspaces.folder_key(w.get("folder", "")), [])[:3]
        # Merge in the server's recent workspaces so folders with no conversations yet still get a card.
        known = {workspaces.folder_key(w.get("folder", "")) for w in spaces}
        for path in workspaces.RECENT_WORKSPACES:
            if workspaces.folder_key(path) in known:
                continue
            known.add(workspaces.folder_key(path))
            spaces.append({"folder": path, "name": workspaces.folder_name(path), "updated": 0,
                           "conversations": 0, "agents": [], "recent": []})
        return {"ok": True, "workspaces": spaces,
                "agents": [{"id": a.id, "name": a.name} for a in agents_mod.active_adapters()]}

    return await asyncio.to_thread(build)


@router.get("/__agents/conversations")
async def agents_conversations(folder: str = "", agent: str = ""):
    if agents_mod is None:
        return _no_agents({"conversations": []})

    def build():
        out = agents_mod.conversations(folder=folder or None, agent=agent or None)
        for c in out:
            c.pop("source", None)
        return {"ok": True, "conversations": out}

    return await asyncio.to_thread(build)


@router.get("/__agents/transcript")
async def agents_transcript(agent: str, cid: str, limit: int = 400):
    if agents_mod is None:
        return JSONResponse({"ok": False, "error": "에이전트 어댑터를 불러오지 못했습니다"},
                            status_code=503)
    messages = await asyncio.to_thread(agents_mod.transcript, agent, cid, limit)
    if not messages:
        return JSONResponse(
            {"ok": False, "error": "기록을 찾지 못했습니다(대화가 삭제되었거나 형식이 다릅니다)."},
            status_code=404)
    return {"ok": True, "messages": messages}


@router.get("/__agents/resume")
async def agents_resume(agent: str, cid: str):
    if agents_mod is None:
        return {"ok": True, "resume": None}
    return {"ok": True, "resume": await asyncio.to_thread(agents_mod.resume, agent, cid)}
