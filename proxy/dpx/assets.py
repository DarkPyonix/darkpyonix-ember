"""Helpers that read static/ files and turn them into responses.

The home, login and wrapper HTML and the overlay CSS/JS all used to be Python strings
inside main.py. No editor could highlight them and main.py kept growing, so they were
moved out into static/. See static/README.md for what each file is.

Files are cached for the lifetime of the process. During development uvicorn --reload
only restarts on .py changes, so to see HTML/CSS/JS edits immediately, start with
DPX_NO_ASSET_CACHE=1.
"""
import os

from fastapi.responses import Response

from dpx.config import STATIC_DIR

NO_CACHE = {"Cache-Control": "no-cache, no-store, must-revalidate"}

_CACHE: dict[str, str] = {}
_CACHE_ENABLED = os.environ.get("DPX_NO_ASSET_CACHE", "") not in ("1", "true", "yes")


def read(name: str) -> str | None:
    """The contents of static/<name>, or None if it does not exist."""
    if _CACHE_ENABLED and name in _CACHE:
        return _CACHE[name]
    path = STATIC_DIR / name
    if not path.is_file():
        return None
    text = path.read_text(encoding="utf-8")
    if _CACHE_ENABLED:
        _CACHE[name] = text
    return text


def _missing(name: str) -> Response:
    return Response(content=f"static/{name} is missing.", status_code=500,
                    media_type="text/plain; charset=utf-8")


def page(name: str, status_code: int = 200) -> Response:
    """Serve one HTML page with no-cache headers."""
    text = read(name)
    if text is None:
        return _missing(name)
    return Response(content=text, status_code=status_code, media_type="text/html",
                    headers=NO_CACHE)


def script(name: str) -> Response:
    text = read(name)
    if text is None:
        return _missing(name)
    return Response(content=text, media_type="application/javascript", headers=NO_CACHE)


def stylesheet(name: str) -> Response:
    text = read(name)
    if text is None:
        return _missing(name)
    return Response(content=text, media_type="text/css", headers=NO_CACHE)
