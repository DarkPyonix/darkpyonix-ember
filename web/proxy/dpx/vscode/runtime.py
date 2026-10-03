"""The two VS Code runtimes behind the IDE window (`INTENT.md` D10, SPEC `FR-W4`):

| Runtime | Server                                                | Marketplace       | Default |
|---------|-------------------------------------------------------|-------------------|---------|
| `ose`   | Code-OSS web server built by DarkPyonix (REH web)      | Open VSX          | yes     |
| `vsc`   | the user's Microsoft VS Code, `code serve-web`         | Microsoft         | —       |

The OSE build pipeline is separate work. Here OSE is just a configured binary:
`DPX_OSE_SERVER` (its launcher, e.g. `<build>/bin/code-server-oss`) and optionally
`DPX_OSE_ARGS`, an argument template with `{host}`, `{port}` and `{data_dir}` placeholders.

This module detects a runtime (`detect`), builds the command that starts its server
(`server_cmd`) and installs the default extensions into it (`ensure_extensions`, `FR-W6`).
Pure stdlib (subprocess, zipfile, json), so it is testable without FastAPI and without a real
`code`: every external call goes through `subprocess.run`, which tests mock.

Detection answers INTEGRATION.md's "verify `code --version`" step and returns install
guidance when the runtime is missing. The UI for that flow lives in the client (later); this
only exposes the data — `GET /__runtime` on the proxy and `python -m dpx.serve --check`.

Default extensions: each entry is a marketplace id (`publisher.name`) or a `.vsix` path,
installed with `<command> --install-extension <entry> --extensions-dir <data-dir>/extensions`.
That is where the server started with `--server-data-dir <data-dir>` loads extensions from
(the VS Code server's default extensions dir is `<server-data-dir>/extensions`). For VSC the
command is the desktop `code` (Microsoft Marketplace); for OSE it is the OSE server binary,
whose CLI installs from the gallery its product.json names (Open VSX). An entry already in
that directory is skipped, so a start after the first does not touch the network.
"""
from __future__ import annotations

import json
import logging
import os
import platform
import shlex
import shutil
import subprocess
import zipfile
from pathlib import Path

logger = logging.getLogger("proxy")

RUNTIMES = ("ose", "vsc")
DEFAULT_RUNTIME = "ose"

# The theme extension FR-W6 asks for. It is not published yet, so the id is a placeholder:
# override with DPX_DEFAULT_EXTENSIONS (os.pathsep-separated ids or .vsix paths, or empty to
# install nothing). Installing a missing id fails with a warning; the server still starts.
DEFAULT_EXTENSIONS = ("darkpyonix.vscode-darkpyonix-theme",)

# The Code-OSS REH web server's own flags (the same server `code serve-web` downloads).
DEFAULT_OSE_ARGS = ("--host {host} --port {port} --without-connection-token "
                    "--accept-server-license-terms --server-data-dir {data_dir}")

CHECK_TIMEOUT = 15          # `code --version` — the macOS shim launches Electron's CLI
INSTALL_TIMEOUT = 180       # one marketplace download

VSC_GUIDE = {
    "Darwin": [
        "Install Visual Studio Code from https://code.visualstudio.com/download "
        "(or: brew install --cask visual-studio-code).",
        "Open VS Code, run the command 'Shell Command: Install 'code' command in PATH'.",
        "Verify in a new terminal: code --version",
    ],
    "Linux": [
        "Install Visual Studio Code from https://code.visualstudio.com/download "
        "(.deb / .rpm / snap: sudo snap install code --classic).",
        "Verify: code --version",
    ],
    "Windows": [
        "Install Visual Studio Code from https://code.visualstudio.com/download "
        "(or: winget install Microsoft.VisualStudioCode).",
        "Keep 'Add to PATH' checked in the installer, then open a new terminal.",
        "Verify: code --version",
    ],
}
OSE_GUIDE = [
    "The OSE runtime is the DarkPyonix-built Code-OSS web server.",
    "Set DPX_OSE_SERVER to its launcher (e.g. <build>/bin/code-server-oss), "
    "or choose the VSC runtime (--runtime vsc / DPX_RUNTIME=vsc).",
    "Verify: <launcher> --version",
]


def default_runtime() -> str:
    rt = (os.environ.get("DPX_RUNTIME") or DEFAULT_RUNTIME).strip().lower()
    return rt if rt in RUNTIMES else DEFAULT_RUNTIME


def runtime_command(rt: str) -> str | None:
    """The command for a runtime: VSC → DPX_CODE_BIN or `code`; OSE → DPX_OSE_SERVER."""
    if rt == "vsc":
        return os.environ.get("DPX_CODE_BIN") or "code"
    return os.environ.get("DPX_OSE_SERVER") or None


def guide(rt: str, system: str | None = None) -> list[str]:
    if rt == "ose":
        return list(OSE_GUIDE)
    return list(VSC_GUIDE.get(system or platform.system(), VSC_GUIDE["Linux"]))


def detect(rt: str | None = None, command: str | None = None) -> dict:
    """Is this runtime usable here?

    Returns `{"runtime", "installed", "command", "path", "version", "commit", "arch",
    "error", "install_guide"}`; `install_guide` is non-empty only when not installed.
    """
    rt = rt or default_runtime()
    command = command or runtime_command(rt)
    out: dict = {"runtime": rt, "installed": False, "command": command, "path": None,
                 "version": None, "commit": None, "arch": None, "error": None,
                 "install_guide": []}
    if rt not in RUNTIMES:
        out["error"] = f"unknown runtime {rt!r} (expected one of {', '.join(RUNTIMES)})"
        return out
    if not command:
        out["error"] = "DPX_OSE_SERVER is not set"
        out["install_guide"] = guide(rt)
        return out
    path = shutil.which(command)
    if path is None:
        out["error"] = f"'{command}' was not found (not on PATH and not an executable file)"
        out["install_guide"] = guide(rt)
        return out
    out["path"] = path
    try:
        proc = subprocess.run([path, "--version"], capture_output=True, text=True,
                              timeout=CHECK_TIMEOUT)
    except (OSError, subprocess.TimeoutExpired) as exc:
        out["error"] = f"'{command} --version' failed: {exc}"
        out["install_guide"] = guide(rt)
        return out
    lines = [ln.strip() for ln in (proc.stdout or "").splitlines() if ln.strip()]
    if proc.returncode != 0 or not lines:
        out["error"] = (f"'{command} --version' exited {proc.returncode}: "
                        f"{(proc.stderr or proc.stdout or '').strip()[:300]}")
        out["install_guide"] = guide(rt)
        return out
    # `--version` prints version, commit, architecture — one per line (both runtimes).
    out["version"] = lines[0]
    out["commit"] = lines[1] if len(lines) > 1 else None
    out["arch"] = lines[2] if len(lines) > 2 else None
    out["installed"] = True
    return out


def server_cmd(rt: str, path: str, host: str, port: int, data_dir: Path,
               ose_args: str | None = None) -> list[str]:
    """The command that starts the runtime's web server on host:port."""
    if rt == "vsc":
        return [path, "serve-web", "--host", host, "--port", str(port),
                "--without-connection-token", "--accept-server-license-terms",
                "--server-data-dir", str(data_dir)]
    template = ose_args if ose_args is not None else (os.environ.get("DPX_OSE_ARGS")
                                                      or DEFAULT_OSE_ARGS)
    args = [a.format(host=host, port=port, data_dir=str(data_dir))
            for a in shlex.split(template)]
    return [path, *args]


# --- Default extensions (FR-W6) ---------------------------------------------------------

def default_extensions() -> list[str]:
    """DPX_DEFAULT_EXTENSIONS if set (empty = none), else DEFAULT_EXTENSIONS."""
    raw = os.environ.get("DPX_DEFAULT_EXTENSIONS")
    if raw is None:
        return list(DEFAULT_EXTENSIONS)
    return [e.strip() for e in raw.split(os.pathsep) if e.strip()]


def extension_id(entry: str) -> str:
    """The `publisher.name` id of an entry: the id itself, or read from a .vsix's
    `extension/package.json`. Lower-cased, as VS Code compares ids case-insensitively."""
    if entry.lower().endswith(".vsix"):
        with zipfile.ZipFile(entry) as z:
            manifest = json.loads(z.read("extension/package.json").decode("utf-8"))
        return f"{manifest['publisher']}.{manifest['name']}".lower()
    return entry.strip().lower()


def installed_ids(extensions_dir: Path) -> set[str]:
    """Ids present in an extensions directory. Installed extensions live in folders named
    `<publisher>.<name>-<version>[-<platform>]`; the id is everything before the version."""
    found: set[str] = set()
    if not extensions_dir.is_dir():
        return found
    for child in extensions_dir.iterdir():
        if not child.is_dir() or child.name.startswith("."):
            continue
        name = child.name.lower()
        cut = next((i for i in range(len(name) - 1)
                    if name[i] == "-" and name[i + 1].isdigit()), len(name))
        found.add(name[:cut])
    return found


def prepare_data_dir(rt: str, data_dir: Path) -> None:
    """Machine settings the runtime needs before it starts.

    OSE (Code-OSS) has no Marketplace signature verifier — `@vscode/vsce-sign` is Microsoft-only —
    so every gallery install fails with "Signature verification was not executed" unless
    `extensions.verifySignature` is off. Existing machine settings are kept; only that key is set.
    VSC is left as the user configured it.
    """
    if rt != "ose":
        return
    # The running server reads Machine settings; its CLI (`--install-extension`, used by
    # ensure_extensions) reads the default profile's User settings.
    for scope in ("Machine", "User"):
        path = data_dir / "data" / scope / "settings.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        try:
            current = json.loads(path.read_text(encoding="utf-8")) if path.exists() else {}
            if not isinstance(current, dict):
                current = {}
        except (OSError, ValueError):
            current = {}
        if current.get("extensions.verifySignature") is False:
            continue
        current["extensions.verifySignature"] = False
        path.write_text(json.dumps(current, indent=2) + "\n", encoding="utf-8")


def ensure_extensions(entries: list[str], extensions_dir: Path, command: str) -> dict:
    """Install every entry not yet in `extensions_dir`. Idempotent; never raises.

    Returns `{"installed": [...], "skipped": [...], "failed": [{"entry":…, "error":…}]}`.
    """
    result: dict = {"installed": [], "skipped": [], "failed": []}
    if not entries:
        return result
    extensions_dir.mkdir(parents=True, exist_ok=True)
    present = installed_ids(extensions_dir)
    for entry in entries:
        try:
            ext_id = extension_id(entry)
        except (OSError, KeyError, ValueError, zipfile.BadZipFile) as exc:
            result["failed"].append({"entry": entry, "error": f"unreadable .vsix: {exc}"})
            continue
        if ext_id in present:
            result["skipped"].append(ext_id)
            continue
        cmd = [command, "--install-extension", entry, "--extensions-dir", str(extensions_dir)]
        try:
            proc = subprocess.run(cmd, capture_output=True, text=True, timeout=INSTALL_TIMEOUT)
        except (OSError, subprocess.TimeoutExpired) as exc:
            result["failed"].append({"entry": entry, "error": str(exc)})
            continue
        if proc.returncode != 0:
            msg = (proc.stderr or proc.stdout or "").strip()[:300]
            result["failed"].append({"entry": entry, "error": msg or f"exit {proc.returncode}"})
            continue
        result["installed"].append(ext_id)
        present.add(ext_id)
    for f in result["failed"]:
        logger.warning("default extension %s not installed: %s", f["entry"], f["error"])
    return result
