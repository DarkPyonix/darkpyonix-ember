"""The Codex CLI adapter (experimental).

Sessions are stored at
    ~/.codex/sessions/YYYY/MM/DD/rollout-<timestamp>-<id>.jsonl

Unlike Claude Code, this format changes fairly often, so unrecognized lines are skipped
silently and only what can be read is shown — a parse failure must not take the whole list
down with it. The workspace path comes from session_meta's cwd, falling back to a cwd found
anywhere in the payload.
"""
from __future__ import annotations

from pathlib import Path

from .base import (
    AgentAdapter, Conversation, Message,
    clip, flatten_content, home_dir, iter_json, norm_folder, pick_preview,
    read_head_lines, read_tail_lines,
)
from .claude_code import _ts


def _payload(obj: dict) -> dict:
    p = obj.get("payload")
    return p if isinstance(p, dict) else obj


def _message_of(obj: dict) -> Message | None:
    p = _payload(obj)
    if p.get("type") not in ("message", "response_item", None):
        if p.get("role") is None:
            return None
    role = p.get("role")
    if role not in ("user", "assistant", "system"):
        return None
    text, kind = flatten_content(p.get("content"))
    if not text.strip():
        return None
    return Message(role=role, text=text, ts=_ts(obj.get("timestamp") or p.get("timestamp")), kind=kind)


class CodexAdapter(AgentAdapter):
    id = "codex"
    name = "Codex"

    def __init__(self, root: Path | None = None):
        self.root = Path(root) if root else (home_dir() / ".codex" / "sessions")
        self._cache: dict[str, tuple[float, int, Conversation]] = {}

    def available(self) -> bool:
        return self.root.is_dir()

    def _files(self) -> list[Path]:
        if not self.available():
            return []
        try:
            return [p for p in self.root.rglob("*.jsonl") if p.is_file()]
        except Exception:
            return []

    def _find_file(self, cid: str) -> Path | None:
        if not cid or "/" in cid or "\\" in cid or ".." in cid:
            return None
        for path in self._files():
            if path.stem == cid:
                return path
        return None

    def _summarize(self, path: Path) -> Conversation | None:
        try:
            st = path.stat()
        except Exception:
            return None
        key = str(path)
        hit = self._cache.get(key)
        if hit and hit[0] == st.st_mtime and hit[1] == st.st_size:
            return hit[2]

        head = read_head_lines(path, 40)
        tail = read_tail_lines(path)
        cwd, created = "", 0.0
        for obj in iter_json(head):
            p = _payload(obj)
            if not cwd and p.get("cwd"):
                cwd = str(p["cwd"])
            if not created:
                created = _ts(obj.get("timestamp") or p.get("timestamp"))

        preview = [m for m in (_message_of(o) for o in iter_json(tail)) if m]
        first_user = next((m for m in preview if m.role == "user"), None)
        title = clip(first_user.text, 60) if first_user else path.stem[:24]
        preview = [Message(m.role, clip(m.text), m.ts, m.kind) for m in pick_preview(preview)]

        conv = Conversation(
            id=path.stem, agent=self.id, folder=cwd or path.parent.name,
            title=title, updated=st.st_mtime, created=created or st.st_mtime,
            message_count=len(preview), preview=preview, source=str(path),
        )
        self._cache[key] = (st.st_mtime, st.st_size, conv)
        return conv

    def conversations(self, folder: str | None = None) -> list[Conversation]:
        want = norm_folder(folder) if folder else None
        out = []
        for path in self._files():
            conv = self._summarize(path)
            if not conv:
                continue
            if want and norm_folder(conv.folder) != want:
                continue
            out.append(conv)
        return sorted(out, key=lambda c: c.updated, reverse=True)

    def transcript(self, cid: str, limit: int = 300) -> list[Message]:
        path = self._find_file(cid)
        if not path:
            return []
        try:
            lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        except Exception:
            return []
        out = [m for m in (_message_of(o) for o in iter_json(lines)) if m]
        return out[-limit:] if limit and len(out) > limit else out

    def resume(self, cid: str) -> dict | None:
        path = self._find_file(cid)
        conv = self._summarize(path) if path else None
        if not conv:
            return None
        return {"kind": "terminal", "command": f"codex resume {cid}", "cwd": conv.folder,
                "hint": "Codex 버전에 따라 명령이 다를 수 있습니다."}
