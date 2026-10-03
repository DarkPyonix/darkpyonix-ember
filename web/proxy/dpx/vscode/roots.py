"""Folder roots: which folders a workspace URL (`?folder=` / `?workspace=`) may open.

With no roots configured (`DPX_FOLDER_ROOTS` unset), every folder is allowed — the original
behaviour of the standalone proxy. The `python -m dpx.serve` launcher always configures at
least one root, and then any folder outside them is refused with 403 before the request
reaches serve-web.

What this does **not** do: sandbox the file system. Once a workspace is open, VS Code's
extension host and terminal run as the OS user that started serve-web and can read anything
that user can. The restriction only decides which workspace a URL can open.

Pure stdlib on purpose, so the rule is testable without FastAPI installed.
"""
from __future__ import annotations

import os
from urllib.parse import unquote, urlsplit

# The query parameters serve-web treats as "open this".
WORKSPACE_PARAMS = ("folder", "workspace")


def parse_roots(value: str | None) -> list[str]:
    """`DPX_FOLDER_ROOTS` → normalised roots. Entries are separated by `os.pathsep`
    (`:` on macOS/Linux, `;` on Windows). Empty entries are ignored."""
    if not value:
        return []
    return [normalise(p) for p in value.split(os.pathsep) if p.strip()]


def normalise(path: str) -> str:
    """An absolute, symlink-resolved, case-normalised (on Windows) form for comparison.

    Resolving symlinks matters: without it a link inside a root that points outside would
    pass the check while VS Code opens the real target."""
    p = os.path.expanduser(path.strip())
    return os.path.normcase(os.path.realpath(p))


def is_allowed(folder: str | None, roots: list[str]) -> bool:
    """May `folder` be opened? Always True when no roots are configured.

    A relative path, an empty value or one that escapes with `..` resolves against the
    filesystem first, so `root/../etc` is judged as `/etc`.
    """
    if not roots:
        return True
    if folder is None or not folder.strip():
        return False
    raw = folder.strip()
    # serve-web also accepts `vscode-remote://…` / `file://…` URIs; only a plain absolute
    # path or a file URI can be checked here, so anything else is refused.
    if raw.startswith("file://"):
        raw = unquote(urlsplit(raw).path)
    elif "://" in raw:
        return False
    if not os.path.isabs(os.path.expanduser(raw)):
        return False
    target = normalise(raw)
    for root in roots:
        try:
            if os.path.commonpath([target, root]) == root:
                return True
        except ValueError:          # different drives on Windows
            continue
    return False


def refused_param(query: dict[str, str] | None, roots: list[str]) -> str | None:
    """The first workspace parameter in `query` whose value is outside `roots`, else None."""
    if not roots or not query:
        return None
    for name in WORKSPACE_PARAMS:
        if name in query and not is_allowed(query.get(name), roots):
            return name
    return None


def roots_from_env() -> list[str]:
    return parse_roots(os.environ.get("DPX_FOLDER_ROOTS"))

