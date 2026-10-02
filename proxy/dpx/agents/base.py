"""The shared agent adapter interface.

Purpose
-------
Every "agent" (Claude Code, Codex, ...) stores its conversation history in a different place
and a different format. The hub and the connector must not have to know about those
differences, so this file defines the shared models and the interface, and each adapter
(agents/claude_code.py and friends) implements only that.

Shared models
-------------
Workspace   : one folder (= a VS Code workspace)
Conversation: one conversation session (metadata for lists and previews)
Message     : one utterance inside a conversation

An adapter only has to implement four things:
    available()            can this agent be used on this machine?
    conversations(folder)  the conversation list (optionally filtered by folder)
    transcript(cid, limit) one conversation's full transcript
    resume(cid)            a description of the "Resume in VS Code" action (a terminal command, etc.)
workspaces() ships a default implementation that groups conversations() by folder, so most
adapters never touch it.
"""
from __future__ import annotations

import json
import os
from dataclasses import dataclass, field, asdict
from pathlib import Path
from typing import Any, Iterator

PREVIEW_MESSAGES = 3       # how many recent utterances to show on a list card
PREVIEW_CHARS = 180        # maximum length of one preview line
TAIL_BYTES = 256 * 1024    # maximum bytes to read from the end of a file, for lists


# --- Shared models -----------------------------------------------------------
@dataclass
class Message:
    role: str = "user"        # user | assistant | system
    text: str = ""
    ts: float = 0.0           # epoch seconds
    kind: str = "text"        # text | tool | meta

    def to_dict(self) -> dict:
        return asdict(self)


@dataclass
class Conversation:
    id: str
    agent: str
    folder: str = ""
    title: str = ""
    updated: float = 0.0
    created: float = 0.0
    message_count: int = 0
    preview: list = field(default_factory=list)   # list[Message]
    source: str = ""                              # the source file path, for debugging

    def to_dict(self) -> dict:
        d = asdict(self)
        d["preview"] = [m.to_dict() if isinstance(m, Message) else dict(m) for m in self.preview]
        return d


@dataclass
class Workspace:
    folder: str
    name: str = ""
    updated: float = 0.0
    conversations: int = 0
    agents: list = field(default_factory=list)    # list[str] of adapter ids

    def to_dict(self) -> dict:
        return asdict(self)


# --- Utilities ---------------------------------------------------------------
def folder_name(path: str) -> str:
    """The last segment of a path (= the workspace name). Handles Windows and POSIX separators."""
    s = str(path or "").replace("\\", "/").rstrip("/")
    return s.split("/")[-1] or s


def norm_folder(path: str) -> str:
    """A normalized key for comparing folders, absorbing separator, case and trailing-slash differences."""
    s = str(path or "").replace("\\", "/").rstrip("/")
    return s.lower()


def clip(text: str, limit: int = PREVIEW_CHARS) -> str:
    t = " ".join(str(text or "").split())
    return t if len(t) <= limit else t[: limit - 1] + "…"


def read_tail_lines(path: Path, max_bytes: int = TAIL_BYTES) -> list[str]:
    """Reads at most max_bytes from the end of the file and returns the complete lines.

    Parsing hundreds of session files in full, just to render a list, is slow enough to be
    felt on a phone. A list only needs the last few utterances, so only the tail is read.
    """
    try:
        size = path.stat().st_size
        with path.open("rb") as f:
            if size > max_bytes:
                f.seek(size - max_bytes)
                f.readline()          # discard the truncated first line
            data = f.read()
    except Exception:
        return []
    return data.decode("utf-8", "replace").splitlines()


def read_head_lines(path: Path, count: int = 40) -> list[str]:
    out = []
    try:
        with path.open("r", encoding="utf-8", errors="replace") as f:
            for line in f:
                out.append(line.rstrip("\n"))
                if len(out) >= count:
                    break
    except Exception:
        return []
    return out


def iter_json(lines: list[str]) -> Iterator[dict]:
    for line in lines:
        line = line.strip()
        if not line or line[0] != "{":
            continue
        try:
            obj = json.loads(line)
        except Exception:
            continue
        if isinstance(obj, dict):
            yield obj


def count_lines(path: Path) -> int:
    try:
        with path.open("rb") as f:
            return sum(1 for _ in f)
    except Exception:
        return 0


def flatten_content(content: Any) -> tuple[str, str]:
    """Flattens a message's content into (display text, kind).

    In both the Anthropic and OpenAI shapes, content is either a string or a list of blocks.
    Blocks mix text / tool_use / tool_result / thinking and so on, and tool calls have to be
    summarized to one line to be readable in a list.
    """
    if content is None:
        return "", "text"
    if isinstance(content, str):
        return content, "text"
    if isinstance(content, dict):
        content = [content]
    if not isinstance(content, list):
        return str(content), "text"

    texts, tools = [], []
    for block in content:
        if isinstance(block, str):
            texts.append(block)
            continue
        if not isinstance(block, dict):
            continue
        btype = block.get("type") or ""
        if btype in ("text", "input_text", "output_text", "summary_text"):
            texts.append(str(block.get("text") or ""))
        elif btype in ("tool_use", "function_call", "local_shell_call", "custom_tool_call"):
            name = block.get("name") or block.get("tool_name") or "tool"
            arg = ""
            args = block.get("input") or block.get("arguments") or {}
            if isinstance(args, str):
                arg = clip(args, 60)
            elif isinstance(args, dict):
                for key in ("file_path", "path", "command", "pattern", "query", "url", "description"):
                    if args.get(key):
                        arg = clip(str(args[key]), 60)
                        break
            tools.append(f"{name}({arg})" if arg else str(name))
        elif btype in ("tool_result", "function_call_output", "local_shell_call_output"):
            tools.append("↩ 결과")
        elif btype == "thinking":
            continue
    if texts and any(t.strip() for t in texts):
        return "\n".join(t for t in texts if t.strip()), "text"
    if tools:
        return "🔧 " + ", ".join(tools[:3]), "tool"
    return "", "text"


# --- The adapter interface ---------------------------------------------------
class AgentAdapter:
    id = "base"
    name = "Agent"

    def available(self) -> bool:
        """Does this machine have any history for this agent?"""
        return False

    def conversations(self, folder: str | None = None) -> list[Conversation]:
        """The conversation list, most recent first. Pass folder to filter to that workspace."""
        return []

    def transcript(self, cid: str, limit: int = 300) -> list[Message]:
        """One conversation's full transcript, oldest to newest. limit trims from the end."""
        return []

    def resume(self, cid: str) -> dict | None:
        """A description of the "Resume" action.

        {"kind": "terminal", "command": "claude --resume <id>", "cwd": folder}
        Returning this shape makes the home screen open the workspace and offer or copy the command.
        """
        return None

    # The default implementation, grouped by folder — adapters need not override it
    def workspaces(self) -> list[Workspace]:
        by_folder: dict[str, Workspace] = {}
        for conv in self.conversations():
            if not conv.folder:
                continue
            key = norm_folder(conv.folder)
            ws = by_folder.get(key)
            if ws is None:
                ws = Workspace(folder=conv.folder, name=folder_name(conv.folder), agents=[self.id])
                by_folder[key] = ws
            ws.conversations += 1
            ws.updated = max(ws.updated, conv.updated)
        return sorted(by_folder.values(), key=lambda w: w.updated, reverse=True)


def pick_preview(msgs: list, count: int = PREVIEW_MESSAGES) -> list:
    """Picks the utterances to preview.

    A card listing nothing but tool calls and results has nothing to read. Prefer utterances
    containing real prose, and fall back to tool lines only when there are none.
    """
    texts = [m for m in msgs if getattr(m, "kind", "text") == "text"]
    chosen = texts if texts else msgs
    return chosen[-count:]


def home_dir() -> Path:
    """The user home, defensive against HOME being empty when the connector runs as a service."""
    for env in ("DPX_HOME", "USERPROFILE", "HOME"):
        v = os.environ.get(env)
        if v and Path(v).is_dir():
            return Path(v)
    return Path.home()
