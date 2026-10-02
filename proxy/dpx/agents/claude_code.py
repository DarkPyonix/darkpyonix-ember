"""The Claude Code adapter.

Sessions are stored at
--------------
    ~/.claude/projects/<the workspace path, slugified into a folder name>/<session-uuid>.jsonl

It is JSONL, one event per line. The fields we use are:
    {"type":"user"|"assistant", "message":{"role":..,"content":..},
     "cwd":"C:\\path\\to\\ws", "sessionId":"...", "timestamp":"ISO8601",
     "isSidechain":bool, "isMeta":bool}
    {"type":"summary", "summary":"<conversation title>", "leafUuid":"..."}

The folder name is a slug, so the original path cannot be recovered from it (separators and
hyphens are flattened together). **Always use the `cwd` field inside the file** as the
workspace path; the slug is a last resort.

Building a list never parses a whole file. Only the first few lines (cwd and title) and the
tail (recent utterances) are read, and the result is cached against (mtime, size).
"""
from __future__ import annotations

import re
from datetime import datetime
from pathlib import Path

from .base import (
    AgentAdapter, Conversation, Message,
    clip, count_lines, flatten_content, home_dir,
    iter_json, norm_folder, pick_preview, read_head_lines, read_tail_lines,
)

ISO_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T")


def _ts(value) -> float:
    """An ISO8601 string or a number, as an epoch."""
    if isinstance(value, (int, float)):
        return float(value)
    if isinstance(value, str) and ISO_RE.match(value):
        try:
            return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()
        except Exception:
            return 0.0
    return 0.0


def _is_noise(obj: dict) -> bool:
    """Lines to exclude from a preview: subagent (sidechain) lines, meta, and slash-command shells."""
    if obj.get("isSidechain") or obj.get("isMeta"):
        return True
    msg = obj.get("message") or {}
    content = msg.get("content")
    if isinstance(content, str) and content.startswith("<command-"):
        return True
    return False


def _message_of(obj: dict) -> Message | None:
    mtype = obj.get("type")
    if mtype not in ("user", "assistant"):
        return None
    msg = obj.get("message") or {}
    text, kind = flatten_content(msg.get("content"))
    if not text.strip():
        return None
    role = msg.get("role") or mtype
    return Message(role=role, text=text, ts=_ts(obj.get("timestamp")), kind=kind)


class ClaudeCodeAdapter(AgentAdapter):
    id = "claude-code"
    name = "Claude Code"

    def __init__(self, root: Path | None = None):
        self.root = Path(root) if root else (home_dir() / ".claude" / "projects")
        self._cache: dict[str, tuple[float, int, Conversation]] = {}

    # --- Discovery ----------------------------------------------------------
    def available(self) -> bool:
        return self.root.is_dir()

    def _files(self) -> list[Path]:
        if not self.available():
            return []
        try:
            return [p for p in self.root.glob("*/*.jsonl") if p.is_file()]
        except Exception:
            return []

    def _find_file(self, cid: str) -> Path | None:
        if not cid or "/" in cid or "\\" in cid or ".." in cid:
            return None
        for path in self._files():
            if path.stem == cid:
                return path
        return None

    # --- Listing ------------------------------------------------------------
    def _summarize(self, path: Path) -> Conversation | None:
        try:
            st = path.stat()
        except Exception:
            return None
        key = str(path)
        hit = self._cache.get(key)
        if hit and hit[0] == st.st_mtime and hit[1] == st.st_size:
            return hit[2]

        head = read_head_lines(path, 60)
        tail = read_tail_lines(path)

        cwd, created, title = "", 0.0, ""
        for obj in iter_json(head):
            if not cwd and obj.get("cwd"):
                cwd = str(obj["cwd"])
            if not created:
                created = _ts(obj.get("timestamp"))
            if not title and obj.get("type") == "summary" and obj.get("summary"):
                title = str(obj["summary"])
        if not title:      # a summary line is sometimes appended at the end, after the conversation
            for obj in iter_json(tail):
                if obj.get("type") == "summary" and obj.get("summary"):
                    title = str(obj["summary"])
                    break
        if not cwd:
            for obj in iter_json(tail):
                if obj.get("cwd"):
                    cwd = str(obj["cwd"])
                    break
        if not cwd:        # last resort: the slug folder name as-is
            cwd = path.parent.name

        preview: list[Message] = []
        for obj in iter_json(tail):
            if _is_noise(obj):
                continue
            msg = _message_of(obj)
            if msg:
                preview.append(msg)
        if not title:
            first_user = next((m for m in preview if m.role == "user" and m.kind == "text"), None)
            title = clip(first_user.text, 60) if first_user else path.stem[:8]
        preview = [Message(m.role, clip(m.text), m.ts, m.kind) for m in pick_preview(preview)]

        conv = Conversation(
            id=path.stem,
            agent=self.id,
            folder=cwd,
            title=clip(title, 90),
            updated=st.st_mtime,
            created=created or st.st_mtime,
            message_count=self._count_messages(path, st.st_size),
            preview=preview,
            source=str(path),
        )
        self._cache[key] = (st.st_mtime, st.st_size, conv)
        return conv

    @staticmethod
    def _count_messages(path: Path, size: int) -> int:
        """The number of user/assistant lines. Files over 8MB are approximated by line count."""
        if size > 8 * 1024 * 1024:
            return count_lines(path)
        try:
            data = path.read_bytes()
        except Exception:
            return 0
        n = 0
        for token in (b'"type":"user"', b'"type": "user"', b'"type":"assistant"', b'"type": "assistant"'):
            n += data.count(token)
        return n

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

    # --- Detail -------------------------------------------------------------
    def transcript(self, cid: str, limit: int = 300) -> list[Message]:
        path = self._find_file(cid)
        if not path:
            return []
        try:
            lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        except Exception:
            return []
        out: list[Message] = []
        for obj in iter_json(lines):
            if obj.get("isSidechain") or obj.get("isMeta"):
                continue
            msg = _message_of(obj)
            if msg:
                out.append(msg)
        return out[-limit:] if limit and len(out) > limit else out

    def resume(self, cid: str) -> dict | None:
        conv = None
        path = self._find_file(cid)
        if path:
            conv = self._summarize(path)
        if not conv:
            return None
        return {
            "kind": "terminal",
            "command": f"claude --resume {cid}",
            "cwd": conv.folder,
            "hint": "터미널에서 이 명령을 실행하면 대화가 그대로 이어집니다.",
        }
