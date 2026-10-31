"""Pure helpers for the `/__terms` relay: no FastAPI, no network (unit-tested on their own).

- `discover()` finds the ember node daemon on this computer (endpoint file or overrides).
- `sanitize_create()` / `sanitize_query()` / `sanitize_control()` decide what a browser may
  send. Everything else (allowed roots, argv parsing) is enforced by ember node itself.
"""
from __future__ import annotations

import json
import re
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import urlsplit

LOCAL_ENDPOINT_FILE = "local.json"

# What started a session (FR-P6). A browser may only claim an IDE origin: `agent` sessions are
# started by ember server's tool calls and `user` ones by hand (ember-term), never by a page.
ORIGINS = ("ide-vscode", "ide-ember", "agent", "user")
BROWSER_ORIGINS = ("ide-vscode", "ide-ember")

_CREATE_KEYS = ("program", "cwd", "env", "env_clear", "size", "origin", "project", "title", "key", "tags")
_LIST_KEYS = ("project", "origin", "running")
# Session ids are 32 hex characters (uuid simple); be a little lenient but never allow `/`.
_ID = re.compile(r"^[A-Za-z0-9_-]{1,64}$")


@dataclass(frozen=True)
class NodeEndpoint:
    url: str
    token: str

    def http(self, path: str) -> str:
        return self.url.rstrip("/") + path

    def ws(self, path: str) -> str:
        base = self.url.rstrip("/")
        if base.startswith("https://"):
            base = "wss://" + base[len("https://"):]
        elif base.startswith("http://"):
            base = "ws://" + base[len("http://"):]
        return base + path

    def headers(self) -> dict[str, str]:
        return {"authorization": f"Bearer {self.token}"}


def discover(url: str, token: str, state_dir: Path) -> NodeEndpoint | None:
    """`url` + `token` if both are set, else `<state_dir>/local.json` (with either override
    applied). None if the daemon cannot be found."""
    if url and token:
        return NodeEndpoint(url, token)
    try:
        data = json.loads((Path(state_dir) / LOCAL_ENDPOINT_FILE).read_text())
    except (OSError, ValueError):
        return None
    if not isinstance(data, dict):
        return None
    found_url = url or data.get("url")
    found_token = token or data.get("token")
    if not isinstance(found_url, str) or not isinstance(found_token, str) or not found_url or not found_token:
        return None
    return NodeEndpoint(found_url, found_token)


def valid_id(term_id: str) -> bool:
    return bool(_ID.match(term_id or ""))


def _size(v) -> dict | None:
    if v is None:
        return None
    if not isinstance(v, dict):
        raise ValueError("size must be {rows, cols}")
    rows, cols = v.get("rows"), v.get("cols")
    if not all(isinstance(x, int) and not isinstance(x, bool) and 0 < x <= 1000 for x in (rows, cols)):
        raise ValueError("size.rows and size.cols must be integers in 1..1000")
    return {"rows": rows, "cols": cols}


def _str_map(v, name: str) -> dict:
    if v is None:
        return {}
    if not isinstance(v, dict) or not all(isinstance(k, str) and isinstance(x, str) for k, x in v.items()):
        raise ValueError(f"{name} must be an object of strings")
    return dict(v)


def sanitize_create(body, home: str) -> dict:
    """The `POST /terms` body to send for a browser's request. Raises ValueError.

    Unknown keys are dropped; `origin` defaults to `ide-vscode` and must be an IDE origin;
    `cwd` defaults to the home directory of the proxy's user (the companion has no folder when
    no workspace is open)."""
    if not isinstance(body, dict):
        raise ValueError("expected a JSON object")
    out = {k: body[k] for k in _CREATE_KEYS if k in body and body[k] is not None}
    origin = out.get("origin", "ide-vscode")
    if origin not in BROWSER_ORIGINS:
        raise ValueError(f"origin must be one of {', '.join(BROWSER_ORIGINS)}")
    out["origin"] = origin
    cwd = out.get("cwd") or home
    if not isinstance(cwd, str):
        raise ValueError("cwd must be a string")
    out["cwd"] = cwd
    if "size" in out:
        out["size"] = _size(out["size"])
    if "env" in out:
        out["env"] = _str_map(out["env"], "env")
    if "tags" in out:
        out["tags"] = _str_map(out["tags"], "tags")
    for k in ("project", "title", "key"):
        if k in out and not isinstance(out[k], str):
            raise ValueError(f"{k} must be a string")
    if "env_clear" in out and not isinstance(out["env_clear"], bool):
        raise ValueError("env_clear must be a boolean")
    program = out.get("program")
    if program is not None:
        # proto::Program: {"argv": [...]} or {"shell": "..."} (validated again by the node).
        if not isinstance(program, dict) or len(program) != 1:
            raise ValueError("program must be {argv: [...]} or {shell: \"...\"}")
    return out


def sanitize_query(params) -> dict:
    """Only the listing filters the node understands."""
    out = {}
    for k in _LIST_KEYS:
        v = params.get(k)
        if v is None or v == "":
            continue
        if k == "origin" and v not in ORIGINS:
            raise ValueError(f"origin must be one of {', '.join(ORIGINS)}")
        if k == "running":
            if v not in ("true", "false"):
                raise ValueError("running must be true or false")
        out[k] = v
    return out


def sanitize_control(body) -> dict:
    if not isinstance(body, dict):
        raise ValueError("expected a JSON object")
    client, take = body.get("client"), body.get("take")
    if not isinstance(client, int) or isinstance(client, bool) or client < 0:
        raise ValueError("client must be a non-negative integer")
    if not isinstance(take, bool):
        raise ValueError("take must be a boolean")
    return {"client": client, "take": take}


def sanitize_kill(body) -> dict:
    if body in (None, b"", ""):
        return {}
    if not isinstance(body, dict):
        raise ValueError("expected a JSON object")
    sig = body.get("signal")
    if sig is None:
        return {}
    if not isinstance(sig, int) or isinstance(sig, bool) or not 1 <= sig <= 64:
        raise ValueError("signal must be an integer in 1..64")
    return {"signal": sig}


def same_origin(origin: str | None, host: str | None) -> bool:
    """Is a WebSocket's `Origin` the page this proxy served? A missing Origin (non-browser
    client) passes; the session cookie is still required."""
    if not origin:
        return True
    if not host:
        return False
    try:
        return urlsplit(origin).netloc.lower() == host.lower()
    except ValueError:
        return False
