"""Helpers that read static/ files and turn them into responses.

The home, login and wrapper HTML and the overlay CSS/JS all used to be Python strings
inside main.py. No editor could highlight them and main.py kept growing, so they were
moved out into static/. See static/README.md for what each file is.

Files are cached for the lifetime of the process. During development uvicorn --reload
only restarts on .py changes, so to see HTML/CSS/JS edits immediately, start with
DPX_NO_ASSET_CACHE=1.
"""
import hashlib
import os

from fastapi import Request
from fastapi.responses import Response

from dpx.config import STATIC_DIR

NO_CACHE = {"Cache-Control": "no-cache, no-store, must-revalidate"}
STATIC_CACHE_CONTROL = "public, max-age=3600"
PAGE_CACHE_CONTROL = "private, no-cache"
WORKBENCH_CSS_CACHE_CONTROL = "private, max-age=3600"
IMMUTABLE_CACHE_CONTROL = "public, max-age=31536000, immutable"

_CACHE: dict[str, str] = {}
_CACHE_ENABLED = os.environ.get("DPX_NO_ASSET_CACHE", "") not in ("1", "true", "yes")


def etag_for(content: str | bytes) -> str:
    """Return a quoted strong ETag derived from the SHA-256 digest of content."""
    data = content.encode("utf-8") if isinstance(content, str) else content
    return f'"{hashlib.sha256(data).hexdigest()[:16]}"'


def etag_matches(if_none_match: str | None, etag: str) -> bool:
    """Weak ETag comparison for If-None-Match (handles *, lists, and W/ prefixes)."""
    if not if_none_match:
        return False
    target = etag[2:] if etag.startswith("W/") else etag
    for raw in if_none_match.split(","):
        token = raw.strip()
        if token == "*":
            return True
        if token.startswith("W/"):
            token = token[2:]
        if token == target:
            return True
    return False


def cached_response(
    content: str | bytes,
    *,
    media_type: str,
    cache_control: str,
    request: Request | None = None,
    status_code: int = 200,
) -> Response:
    """Build a response with Cache-Control and ETag, answering 304 on If-None-Match."""
    etag = etag_for(content)
    headers = {"Cache-Control": cache_control, "ETag": etag}
    if (
        request is not None
        and request.method in ("GET", "HEAD")
        and etag_matches(request.headers.get("if-none-match"), etag)
    ):
        return Response(status_code=304, headers=headers)
    return Response(content=content, status_code=status_code, media_type=media_type,
                    headers=headers)


def maybe_not_modified(request: Request, response: Response) -> Response:
    """Convert a 200 GET/HEAD response with a matching ETag into 304 Not Modified."""
    if request.method not in ("GET", "HEAD") or response.status_code != 200:
        return response
    etag = response.headers.get("etag")
    if not etag or not etag_matches(request.headers.get("if-none-match"), etag):
        return response
    headers = {"ETag": etag}
    for name in ("cache-control", "vary"):
        val = response.headers.get(name)
        if val is not None:
            headers[name] = val
    return Response(status_code=304, headers=headers)


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


def page(name: str, status_code: int = 200, request: Request | None = None) -> Response:
    """Serve one HTML page with ETag revalidation (or no-store for error pages)."""
    text = read(name)
    if text is None:
        return _missing(name)
    if status_code != 200:
        return Response(content=text, status_code=status_code, media_type="text/html",
                        headers=NO_CACHE)
    return cached_response(text, media_type="text/html", cache_control=PAGE_CACHE_CONTROL,
                           request=request)


def script(name: str, request: Request | None = None) -> Response:
    text = read(name)
    if text is None:
        return _missing(name)
    return cached_response(text, media_type="application/javascript",
                           cache_control=STATIC_CACHE_CONTROL, request=request)


def stylesheet(name: str, request: Request | None = None) -> Response:
    text = read(name)
    if text is None:
        return _missing(name)
    return cached_response(text, media_type="text/css",
                           cache_control=STATIC_CACHE_CONTROL, request=request)
