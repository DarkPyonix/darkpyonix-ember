"""The agent adapter registry.

To support a new agent, write a module implementing base.AgentAdapter and add one line to
_CANDIDATES below. The connector, the hub and the home screen need no changes.
"""
from __future__ import annotations

from .base import AgentAdapter, Conversation, Workspace, norm_folder

_CANDIDATES = []

try:
    from .claude_code import ClaudeCodeAdapter
    _CANDIDATES.append(ClaudeCodeAdapter)
except Exception:                                  # pragma: no cover
    pass
try:
    from .codex import CodexAdapter
    _CANDIDATES.append(CodexAdapter)
except Exception:                                  # pragma: no cover
    pass

_ADAPTERS: list[AgentAdapter] | None = None


def all_adapters() -> list[AgentAdapter]:
    """Every registered adapter, whether or not it is available."""
    global _ADAPTERS
    if _ADAPTERS is None:
        made = []
        for cls in _CANDIDATES:
            try:
                made.append(cls())
            except Exception:
                continue
        _ADAPTERS = made
    return _ADAPTERS


def active_adapters() -> list[AgentAdapter]:
    """Only the adapters that actually have history on this machine."""
    return [a for a in all_adapters() if a.available()]


def get_adapter(agent_id: str) -> AgentAdapter | None:
    for a in all_adapters():
        if a.id == agent_id:
            return a
    return None


def workspaces() -> list[dict]:
    """Every adapter's workspaces, merged by folder, most recent first."""
    merged: dict[str, Workspace] = {}
    for adapter in active_adapters():
        for ws in adapter.workspaces():
            key = norm_folder(ws.folder)
            cur = merged.get(key)
            if cur is None:
                merged[key] = ws
                continue
            cur.conversations += ws.conversations
            cur.updated = max(cur.updated, ws.updated)
            for a in ws.agents:
                if a not in cur.agents:
                    cur.agents.append(a)
    out = sorted(merged.values(), key=lambda w: w.updated, reverse=True)
    return [w.to_dict() for w in out]


def conversations(folder: str | None = None, agent: str | None = None, limit: int = 200) -> list[dict]:
    out: list[Conversation] = []
    for adapter in active_adapters():
        if agent and adapter.id != agent:
            continue
        try:
            out.extend(adapter.conversations(folder))
        except Exception:
            continue
    out.sort(key=lambda c: c.updated, reverse=True)
    return [c.to_dict() for c in out[:limit]]


def transcript(agent: str, cid: str, limit: int = 300) -> list[dict]:
    adapter = get_adapter(agent)
    if not adapter:
        return []
    try:
        return [m.to_dict() for m in adapter.transcript(cid, limit)]
    except Exception:
        return []


def resume(agent: str, cid: str) -> dict | None:
    adapter = get_adapter(agent)
    if not adapter:
        return None
    try:
        return adapter.resume(cid)
    except Exception:
        return None
