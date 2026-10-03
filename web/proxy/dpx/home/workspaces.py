"""The recent workspace list.

Recorded every time a page with `?folder=...` is served, and persisted best-effort to
_xmo_recent.json beside the module. (uvicorn --reload only watches *.py, so writing this
.json does not trigger a restart.)

The home screen's folder list = this record ∪ folders with agent conversations ∪ browser
bookmarks.
"""
import json

from dpx.config import RECENT_FILE, RECENT_LIMIT

try:
    RECENT_WORKSPACES: list[str] = json.loads(RECENT_FILE.read_text(encoding="utf-8"))
    if not isinstance(RECENT_WORKSPACES, list):
        RECENT_WORKSPACES = []
except Exception:
    RECENT_WORKSPACES = []


def record(folder: str | None) -> None:
    global RECENT_WORKSPACES
    if not folder or not folder.strip():
        return
    folder = folder.strip()
    RECENT_WORKSPACES = [w for w in RECENT_WORKSPACES if w != folder]
    RECENT_WORKSPACES.insert(0, folder)
    RECENT_WORKSPACES = RECENT_WORKSPACES[:RECENT_LIMIT]
    try:
        RECENT_FILE.write_text(json.dumps(RECENT_WORKSPACES, ensure_ascii=False), encoding="utf-8")
    except Exception:
        pass


def folder_key(path: str) -> str:
    """A folder comparison key ignoring case and slash differences (for Windows paths)."""
    return str(path or "").replace("\\", "/").rstrip("/").lower()


def folder_name(path: str) -> str:
    """The last segment of the path — the name shown on the card."""
    s = str(path or "").replace("\\", "/").rstrip("/")
    return s.split("/")[-1] or s
