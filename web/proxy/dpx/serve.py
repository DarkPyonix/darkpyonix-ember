"""`python -m dpx.serve` — one non-interactive entry for the IDE window on a computer.

Runs next to ember node on each computer (SPEC `FR-W1`, `FR-W4`):

    1. checks the chosen runtime — OSE (default, `DPX_OSE_SERVER`) or VSC (`code`), see
       dpx/vscode/runtime.py; missing → prints the not-installed status with install
       guidance as JSON and exits 3
    2. installs the default extensions (FR-W6) into the server's data dir, once
    3. starts the VS Code web server (OSE's REH web server, or `code serve-web`) on a free
       loopback port — never exposed directly; the proxy's login replaces its token
    4. starts the proxy (uvicorn main:app) in front of it, with folder roots enforced
    5. prints one JSON line `{"event": "ready", "url": …}` on stdout (and to
       `--announce-file` if given), then supervises both until a signal or until either dies

Run from the `web/proxy/` directory:

    python -m dpx.serve --root ~/work --root ~/src                 # OSE (DPX_OSE_SERVER)
    python -m dpx.serve --runtime vsc --root ~/work                # the user's `code serve-web`
    python -m dpx.serve --runtime vsc --check                      # runtime status as JSON

Only the stdlib is imported here; uvicorn/FastAPI run in the child process. That keeps the
launcher's logic (ports, URLs, commands) testable with a plain `python3`.
"""
from __future__ import annotations

import argparse
import json
import os
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

from dpx.vscode import roots as roots_mod
from dpx.vscode import runtime

PROXY_DIR = Path(__file__).resolve().parent.parent
LOOPBACK = "127.0.0.1"
DEFAULT_DATA_DIR = Path.home() / ".ember" / "vscode-web"
EXIT_NOT_INSTALLED = 3
EXIT_START_FAILED = 4


# --- Pure helpers (unit-tested) ------------------------------------------------------------

def free_port(host: str = LOOPBACK) -> int:
    """A port that was free a moment ago. Another process could take it before the child
    binds it; the child then exits, and the launcher reports a start failure (exit 4)."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind((host, 0))
        return s.getsockname()[1]


def announce_url(host: str, port: int, public_url: str | None = None) -> str:
    """The URL others use to reach the proxy. `--public-url` wins (a tunnel or DNS name);
    a wildcard bind is announced as loopback, since the bind address is not reachable."""
    if public_url:
        return public_url.rstrip("/") + "/"
    shown = LOOPBACK if host in ("0.0.0.0", "", "::") else host
    if ":" in shown and not shown.startswith("["):
        shown = f"[{shown}]"
    return f"http://{shown}:{port}/"


def proxy_cmd(python: str, host: str, port: int) -> list[str]:
    return [python, "-m", "uvicorn", "main:app", "--host", host, "--port", str(port)]


def proxy_env(base: dict[str, str], upstream_port: int, roots: list[str],
              runtime_name: str | None = None, runtime_path: str | None = None) -> dict[str, str]:
    """The proxy child's environment: the upstream it fronts, its folder roots, and the
    runtime it reports on `/__runtime`."""
    env = dict(base)
    if runtime_name:
        env["DPX_RUNTIME"] = runtime_name
        if runtime_path:
            env["DPX_OSE_SERVER" if runtime_name == "ose" else "DPX_CODE_BIN"] = runtime_path
    env["XMO_UPSTREAM_HOST"] = LOOPBACK
    env["XMO_UPSTREAM_PORT"] = str(upstream_port)
    env["DPX_FOLDER_ROOTS"] = os.pathsep.join(roots)
    return env


def resolve_roots(cli_roots: list[str] | None, env_value: str | None) -> list[str]:
    """`--root` entries, else DPX_FOLDER_ROOTS. Each must be an existing directory."""
    raw = cli_roots or ([p for p in (env_value or "").split(os.pathsep) if p.strip()])
    out = []
    for r in raw:
        norm = roots_mod.normalise(r)
        if not os.path.isdir(norm):
            raise ValueError(f"folder root is not a directory: {r}")
        out.append(norm)
    return out


def ready_line(url: str, upstream_port: int, roots: list[str], rt: dict, ext: dict) -> str:
    return json.dumps({"event": "ready", "url": url, "upstream_port": upstream_port,
                       "roots": roots, "runtime": rt, "extensions": ext}, ensure_ascii=False)


# --- Process handling ---------------------------------------------------------------------

def wait_port(port: int, proc: subprocess.Popen, timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            return False
        try:
            with socket.create_connection((LOOPBACK, port), timeout=1):
                return True
        except OSError:
            time.sleep(0.2)
    return False


def wait_http_ok(url: str, proc: subprocess.Popen, timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            return False
        try:
            with urllib.request.urlopen(url, timeout=2) as r:
                if r.status == 200:
                    return True
        except (urllib.error.URLError, OSError):
            pass
        time.sleep(0.2)
    return False


def spawn(cmd: list[str], **kw) -> subprocess.Popen:
    if os.name == "posix":
        kw.setdefault("start_new_session", True)    # own process group: stop() reaps children
    return subprocess.Popen(cmd, **kw)


def stop(proc: subprocess.Popen | None, grace: float = 10.0) -> None:
    if proc is None or proc.poll() is not None:
        return
    try:
        if os.name == "posix":
            os.killpg(proc.pid, signal.SIGTERM)
        else:
            proc.terminate()
        proc.wait(timeout=grace)
    except (ProcessLookupError, PermissionError):
        return
    except subprocess.TimeoutExpired:
        if os.name == "posix":
            os.killpg(proc.pid, signal.SIGKILL)
        else:
            proc.kill()
        proc.wait()


def _log(msg: str) -> None:
    print(f"[dpx.serve] {msg}", file=sys.stderr, flush=True)


def parse_args(argv: list[str] | None) -> argparse.Namespace:
    p = argparse.ArgumentParser(prog="python -m dpx.serve",
                                description="Serve the wrapped VS Code Web (OSE or VSC runtime) on this computer.")
    p.add_argument("--root", action="append", metavar="DIR",
                   help="A folder that may be opened (repeatable). Default: DPX_FOLDER_ROOTS.")
    p.add_argument("--host", default=os.environ.get("DPX_SERVE_HOST", LOOPBACK),
                   help="Proxy bind address (default 127.0.0.1).")
    p.add_argument("--port", type=int, default=int(os.environ.get("DPX_SERVE_PORT", "0")),
                   help="Proxy port; 0 picks a free one (default).")
    p.add_argument("--public-url", default=os.environ.get("DPX_PUBLIC_URL"),
                   help="The URL to announce instead of http://host:port/.")
    p.add_argument("--runtime", choices=runtime.RUNTIMES, default=runtime.default_runtime(),
                   help="ose (default; DPX_RUNTIME) or vsc.")
    p.add_argument("--server", metavar="CMD",
                   help="The runtime's command. Default: DPX_OSE_SERVER for ose, "
                        "DPX_CODE_BIN or `code` for vsc.")
    p.add_argument("--data-dir", type=Path,
                   default=Path(os.environ["DPX_SERVER_DATA_DIR"]) if os.environ.get("DPX_SERVER_DATA_DIR") else None,
                   help="The server's --server-data-dir (settings and extensions live here). "
                        "Default: ~/.ember/vscode-web/<runtime>, one per runtime so the two "
                        "marketplaces' extensions never mix.")
    p.add_argument("--extension", action="append", metavar="ID_OR_VSIX",
                   help="A default extension (repeatable). Default: DPX_DEFAULT_EXTENSIONS, "
                        "else the DarkPyonix theme.")
    p.add_argument("--announce-file", type=Path,
                   help="Also write the ready line here (replaced atomically).")
    p.add_argument("--startup-timeout", type=float, default=180.0,
                   help="Seconds to wait for each process to come up.")
    p.add_argument("--check", action="store_true",
                   help="Print the runtime status as JSON and exit (3 if not installed).")
    return p.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)

    rt = runtime.detect(args.runtime, args.server)
    if args.check or not rt["installed"]:
        print(json.dumps(rt, ensure_ascii=False, indent=None if not args.check else 2), flush=True)
        return 0 if rt["installed"] else EXIT_NOT_INSTALLED

    try:
        roots = resolve_roots(args.root, os.environ.get("DPX_FOLDER_ROOTS"))
    except ValueError as exc:
        _log(str(exc))
        return 2
    if not roots:
        _log("at least one --root (or DPX_FOLDER_ROOTS) is required: "
             "this mode never serves an unrestricted folder")
        return 2

    if args.data_dir is None:
        args.data_dir = DEFAULT_DATA_DIR / args.runtime
    args.data_dir.mkdir(parents=True, exist_ok=True)
    runtime.prepare_data_dir(args.runtime, args.data_dir)
    entries = args.extension if args.extension is not None else runtime.default_extensions()
    ext = runtime.ensure_extensions(entries, args.data_dir / "extensions", rt["path"])

    upstream = proxy = None

    def on_signal(_signum, _frame):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, on_signal)
    try:
        upstream_port = free_port()
        _log(f"starting the {args.runtime} server on {LOOPBACK}:{upstream_port}")
        upstream = spawn(runtime.server_cmd(args.runtime, rt["path"], LOOPBACK, upstream_port,
                                            args.data_dir),
                         stdout=sys.stderr, stderr=sys.stderr)
        if not wait_port(upstream_port, upstream, args.startup_timeout):
            _log(f"the {args.runtime} server did not come up")
            return EXIT_START_FAILED

        port = args.port or free_port(args.host if args.host not in ("0.0.0.0", "::") else LOOPBACK)
        _log(f"starting proxy on {args.host}:{port}")
        proxy = spawn(proxy_cmd(sys.executable, args.host, port), cwd=str(PROXY_DIR),
                      env=proxy_env(dict(os.environ), upstream_port, roots,
                                    args.runtime, rt["path"]),
                      stdout=sys.stderr, stderr=sys.stderr)
        if not wait_http_ok(f"http://{LOOPBACK}:{port}/healthz", proxy, args.startup_timeout):
            _log("the proxy did not come up")
            return EXIT_START_FAILED

        line = ready_line(announce_url(args.host, port, args.public_url), upstream_port,
                          roots, rt, ext)
        print(line, flush=True)
        if args.announce_file:
            tmp = args.announce_file.with_suffix(args.announce_file.suffix + ".tmp")
            tmp.write_text(line + "\n", encoding="utf-8")
            os.replace(tmp, args.announce_file)

        while True:
            for name, proc in ((f"{args.runtime} server", upstream), ("proxy", proxy)):
                if proc.poll() is not None:
                    _log(f"{name} exited with {proc.returncode}; stopping")
                    return EXIT_START_FAILED
            time.sleep(0.5)
    except KeyboardInterrupt:
        _log("stopping")
        return 0
    finally:
        stop(proxy)
        stop(upstream)
        if args.announce_file and args.announce_file.exists():
            try:
                args.announce_file.unlink()
            except OSError:
                pass


if __name__ == "__main__":
    sys.exit(main())
