#!/usr/bin/env python3
"""Diffs a served VS Code Web bundle against its unmodified upstream release.

Verifies that every non-allowlisted asset (JS, fonts, WASM, JSON, icons, internal
HTML frames, secondary CSS) is byte-identical to upstream, and that the three
allowlisted proxy injection points differ only by the exact ember/proxy payloads:

  1. Top-level workbench HTML (`workbench.html` / `index.html`):
     only the viewport meta, `/__overlay.css`, `/__overlay.js`, optional
     `/__detach.js`, and allowlisted `configurationDefaults` entries.
  2. Workbench main CSS (`out/vs/.../workbench*.css`):
     only the appended `ember/proxy/static/overlay.css` block.
  3. Webview host frame (`.../webview/browser/pre/index.html`):
     only the `/__kb.js` script tag inserted before `</head>`.

Usage:
  check-vscode-bundle-diff.py --self-check
  check-vscode-bundle-diff.py --upstream <dir> --served <dir>
  check-vscode-bundle-diff.py --upstream <dir> --served-url <http://host:port>
  check-vscode-bundle-diff.py --upstream <dir>   (runs ember/proxy in-process)
"""
from __future__ import annotations

import argparse
import difflib
import hashlib
import html as html_lib
import importlib
import json
import os
import re
import sys
import tempfile
import threading
import urllib.request
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
PROXY_DIR = REPO_ROOT / "ember" / "proxy"
DEFAULT_OVERLAY_CSS = PROXY_DIR / "static" / "overlay.css"

if str(PROXY_DIR) not in sys.path:
    sys.path.insert(0, str(PROXY_DIR))

from dpx.vscode import html_rewrite  # noqa: E402

MAIN_CSS_REGEX = re.compile(
    r"(?:^|/)out/vs/(?:workbench|code/browser/workbench)/workbench(?:\.(?:web|desktop)\.main(?:\.nls)?(?:\.min)?)?\.css$"
)
WEBVIEW_HOST_SUFFIX = "webview/browser/pre/index.html"
OVERLAY_MARKER = "\n\n/* --- Custom mobile overlay --- */\n"
ASSET_VERSION_RE = re.compile(r"/__(?:overlay\.css|overlay\.js|detach\.js|kb\.js)\?v=([A-Za-z0-9._-]+)")
MAX_DIFF_LINES = 60


def is_main_css(rel_path: str) -> bool:
    return bool(MAIN_CSS_REGEX.search(rel_path))


def is_webview_host(rel_path: str) -> bool:
    return rel_path.endswith(WEBVIEW_HOST_SUFFIX)


def is_workbench_html(rel_path: str) -> bool:
    return (
        rel_path == "workbench.html"
        or rel_path == "index.html"
        or rel_path.endswith("/code/browser/workbench/workbench.html")
    )


def _format_diff(rel_path: str, expected: bytes, actual: bytes, label: str = "upstream") -> str:
    if b"\x00" not in expected and b"\x00" not in actual:
        try:
            exp_text = expected.decode("utf-8")
            act_text = actual.decode("utf-8")
            lines = list(
                difflib.unified_diff(
                    exp_text.splitlines(keepends=True),
                    act_text.splitlines(keepends=True),
                    fromfile=f"{label}/{rel_path}",
                    tofile=f"served/{rel_path}",
                    n=2,
                )
            )
            if len(lines) > MAX_DIFF_LINES:
                lines = lines[:MAX_DIFF_LINES] + [
                    f"... ({len(lines) - MAX_DIFF_LINES} more diff lines truncated)\n"
                ]
            return "".join(lines).rstrip("\n")
        except UnicodeDecodeError:
            pass
    exp_sha = hashlib.sha256(expected).hexdigest()[:16]
    act_sha = hashlib.sha256(actual).hexdigest()[:16]
    return (
        f"binary mismatch for {rel_path}: "
        f"{label} {len(expected)} bytes (sha256:{exp_sha}) vs "
        f"served {len(actual)} bytes (sha256:{act_sha})"
    )


def check_file(rel_path: str, upstream_bytes: bytes, served_bytes: bytes, overlay_css: str) -> str | None:
    """Returns None if `served_bytes` is allowed for `rel_path`, else an error summary."""
    if served_bytes == upstream_bytes:
        return None

    if is_main_css(rel_path):
        try:
            up_text = upstream_bytes.decode("utf-8")
            sv_text = served_bytes.decode("utf-8")
        except UnicodeDecodeError:
            return _format_diff(rel_path, upstream_bytes, served_bytes)
        expected_text = up_text + OVERLAY_MARKER + overlay_css
        if sv_text == expected_text:
            return None
        return (
            f"unexpected CSS modification in {rel_path}:\n"
            + _format_diff(rel_path, expected_text.encode("utf-8"), served_bytes, label="expected")
        )

    if is_workbench_html(rel_path):
        try:
            up_text = upstream_bytes.decode("utf-8")
            sv_text = served_bytes.decode("utf-8")
        except UnicodeDecodeError:
            return _format_diff(rel_path, upstream_bytes, served_bytes)
        match = ASSET_VERSION_RE.search(sv_text)
        version = match.group(1) if match else "0"
        candidates: list[str] = []
        for detach in (True, False):
            for theme in (True, False):
                candidate, _ = html_rewrite.workbench_inserts(
                    up_text, version, detach=detach, theme=theme
                )
                if sv_text == candidate:
                    return None
                candidates.append(candidate)
        best = candidates[0] if "/__detach.js" in sv_text else candidates[3]
        return (
            f"unexpected HTML modification in {rel_path}:\n"
            + _format_diff(rel_path, best.encode("utf-8"), served_bytes, label="expected")
        )

    if is_webview_host(rel_path):
        try:
            up_text = upstream_bytes.decode("utf-8")
            sv_text = served_bytes.decode("utf-8")
        except UnicodeDecodeError:
            return _format_diff(rel_path, upstream_bytes, served_bytes)
        match = re.search(r"/__kb\.js\?v=([A-Za-z0-9._-]+)", sv_text)
        version = match.group(1) if match else "0"
        expected_text = up_text.replace(
            "</head>", f'<script src="/__kb.js?v={version}" defer></script></head>', 1
        )
        if sv_text == expected_text:
            return None
        return (
            f"unexpected webview host HTML modification in {rel_path}:\n"
            + _format_diff(rel_path, expected_text.encode("utf-8"), served_bytes, label="expected")
        )

    return (
        f"unallowlisted asset modification in {rel_path}:\n"
        + _format_diff(rel_path, upstream_bytes, served_bytes, label="upstream")
    )


def collect_files(root: Path) -> dict[str, Path]:
    files: dict[str, Path] = {}
    for path in sorted(root.rglob("*")):
        if path.is_file():
            rel = path.relative_to(root).as_posix()
            files[rel] = path
    return files


def diff_bundles(upstream_dir: Path, served_dir: Path, overlay_css: str) -> list[str]:
    upstream_files = collect_files(upstream_dir)
    served_files = collect_files(served_dir)
    errors: list[str] = []

    for missing in sorted(set(upstream_files) - set(served_files)):
        errors.append(f"missing file in served bundle: {missing}")
    for extra in sorted(set(served_files) - set(upstream_files)):
        errors.append(f"unexpected extra file in served bundle: {extra}")

    for rel in sorted(set(upstream_files) & set(served_files)):
        err = check_file(
            rel,
            upstream_files[rel].read_bytes(),
            served_files[rel].read_bytes(),
            overlay_css,
        )
        if err is not None:
            errors.append(err)
    return errors


def fetch_served_from_url(upstream_dir: Path, served_dir: Path, base_url: str, cookie: str | None) -> None:
    base = base_url.rstrip("/")
    for rel in sorted(collect_files(upstream_dir)):
        url = f"{base}/" if rel == "workbench.html" else f"{base}/{rel}"
        headers = {"Accept": "text/html"} if rel.endswith(".html") else {}
        if cookie:
            headers["Cookie"] = cookie
        req = urllib.request.Request(url, headers=headers)
        with urllib.request.urlopen(req, timeout=30) as resp:
            data = resp.read()
        out = served_dir / rel
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_bytes(data)


def proxy_upstream_in_process(upstream_dir: Path, served_dir: Path) -> bool:
    """Serves `upstream_dir` through ember/proxy's FastAPI app into `served_dir`.

    Returns True if FastAPI was available and used, False otherwise.
    """
    try:
        from fastapi.testclient import TestClient
    except ImportError:
        return False

    workspace_dir = served_dir.parent / "_workspace"
    workspace_dir.mkdir(parents=True, exist_ok=True)

    class UpstreamHandler(SimpleHTTPRequestHandler):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, directory=str(upstream_dir), **kwargs)

        def do_GET(self):
            if self.path == "/" or self.path.startswith("/?"):
                self.path = "/workbench.html"
            super().do_GET()

        def log_message(self, format, *args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), UpstreamHandler)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()

    saved_env = dict(os.environ)
    try:
        os.environ.update(
            {
                "DPX_FOLDER_ROOTS": str(workspace_dir),
                "DPX_USERNAME": "u",
                "DPX_PASSWORD": "p",
                "XMO_UPSTREAM_HOST": "127.0.0.1",
                "XMO_UPSTREAM_PORT": str(port),
            }
        )
        for name in [m for m in sys.modules if m == "main" or m.startswith("dpx")]:
            del sys.modules[name]
        import dpx.config as config

        config.DB_FILE = served_dir.parent / "_proxy_test.db"
        config.RECENT_FILE = served_dir.parent / "_proxy_recent.json"
        main_mod = importlib.import_module("main")

        with TestClient(main_mod.app) as client:
            login = client.post("/auth/login", json={"username": "u", "password": "p"})
            if login.status_code != 200:
                raise RuntimeError(f"proxy login failed: {login.status_code} {login.text}")
            for rel in sorted(collect_files(upstream_dir)):
                if rel == "workbench.html":
                    resp = client.get(
                        "/",
                        params={"folder": str(workspace_dir), "xmo": "embed"},
                        headers={"accept": "text/html"},
                    )
                else:
                    headers = {"accept": "text/html"} if rel.endswith(".html") else {}
                    resp = client.get("/" + rel, headers=headers)
                if resp.status_code != 200:
                    raise RuntimeError(f"proxy request for {rel} returned {resp.status_code}")
                out = served_dir / rel
                out.parent.mkdir(parents=True, exist_ok=True)
                out.write_bytes(resp.content)
        return True
    finally:
        server.shutdown()
        server.server_close()
        os.environ.clear()
        os.environ.update(saved_env)


def _write_fixture_bundle(root: Path, overlay_css: str, *, injected: bool) -> None:
    cfg = {
        "folderUri": {"$mid": 1, "scheme": "vscode-remote", "path": "/workspace"},
        "productConfiguration": {"nameShort": "Code - OSS"},
    }
    wb_html = (
        "<!DOCTYPE html>\n<html>\n<head>\n"
        '<meta charset="utf-8">\n'
        '<meta id="vscode-workbench-web-configuration" data-settings="'
        + html_lib.escape(json.dumps(cfg), quote=True)
        + '">\n'
        "<title>Code - OSS</title>\n"
        "</head>\n<body><div id=\"workbench\"></div></body>\n</html>\n"
    )
    webview_html = (
        "<!DOCTYPE html>\n<html>\n<head><meta charset=\"utf-8\"></head>\n"
        "<body><script src=\"main.js\"></script></body>\n</html>\n"
    )
    worker_html = (
        "<!DOCTYPE html>\n<html>\n<head><title>Worker</title></head>\n"
        "<body><script>/* stock worker host */</script></body>\n</html>\n"
    )
    main_css = ".monaco-workbench { color: #ccc; }\n"
    files: dict[str, bytes] = {
        "workbench.html": wb_html.encode("utf-8"),
        "out/vs/workbench/workbench.web.main.css": main_css.encode("utf-8"),
        "out/vs/workbench/workbench.web.main.js": b"define('workbench', [], function () { return 1; });\n",
        "out/vs/workbench/contrib/webview/browser/pre/index.html": webview_html.encode("utf-8"),
        "out/vs/workbench/services/extensions/worker/webWorkerExtensionHostIframe.html": worker_html.encode("utf-8"),
        "out/media/codicon.ttf": b"\x00\x01\x00\x00codicon-fixture\xff\xfe",
        "node_modules/vscode-oniguruma/release/onig.wasm": b"\x00asm\x01\x00\x00\x00fixture",
        "product.json": b'{"nameShort": "Code - OSS"}\n',
        "resources/code-icon.png": b"\x89PNG\r\n\x1a\nfixture",
    }
    if injected:
        ver = "1700000000"
        injected_wb, _ = html_rewrite.workbench_inserts(wb_html, ver, detach=True, theme=True)
        files["workbench.html"] = injected_wb.encode("utf-8")
        files["out/vs/workbench/workbench.web.main.css"] = (
            main_css + OVERLAY_MARKER + overlay_css
        ).encode("utf-8")
        files["out/vs/workbench/contrib/webview/browser/pre/index.html"] = webview_html.replace(
            "</head>", f'<script src="/__kb.js?v={ver}" defer></script></head>', 1
        ).encode("utf-8")
    for rel, data in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)


def run_self_check(overlay_css: str) -> int:
    with tempfile.TemporaryDirectory() as tmp_str:
        base = Path(tmp_str)
        upstream = base / "upstream"
        served = base / "served"
        _write_fixture_bundle(upstream, overlay_css, injected=False)

        used_proxy = proxy_upstream_in_process(upstream, served)
        if not used_proxy:
            _write_fixture_bundle(served, overlay_css, injected=True)

        errors = diff_bundles(upstream, served, overlay_css)
        if errors:
            print("error: self-check failed on clean fixture bundle:", file=sys.stderr)
            for err in errors:
                print(err, file=sys.stderr)
            return 1

        # Verify that each class of unallowlisted mutation is rejected.
        mutations = [
            ("out/vs/workbench/workbench.web.main.js", lambda b: b + b"// mutated\n"),
            ("node_modules/vscode-oniguruma/release/onig.wasm", lambda b: b + b"\x00"),
            ("workbench.html", lambda b: b.replace(b"</body>", b"<script src=\"/x.js\"></script></body>")),
            ("out/vs/workbench/workbench.web.main.css", lambda b: b + b"\n.extra { color: red; }\n"),
            (
                "out/vs/workbench/contrib/webview/browser/pre/index.html",
                lambda b: b.replace(b"</head>", b"<script src=\"/__overlay.js?v=1\"></script></head>"),
            ),
            (
                "out/vs/workbench/services/extensions/worker/webWorkerExtensionHostIframe.html",
                lambda b: b.replace(b"</head>", b"<script src=\"/__overlay.js\"></script></head>"),
            ),
        ]
        for rel, mutate in mutations:
            target = served / rel
            original = target.read_bytes()
            try:
                target.write_bytes(mutate(original))
                mut_errors = diff_bundles(upstream, served, overlay_css)
                if not mut_errors:
                    print(f"error: self-check did not catch mutation in {rel}", file=sys.stderr)
                    return 1
            finally:
                target.write_bytes(original)

    mode = "live ember/proxy" if used_proxy else "stdlib fixture"
    print(f"ok: vscode bundle diff self-check passed ({mode}, {len(mutations)} negative checks)")
    return 0


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Verify a served VS Code Web bundle matches its pinned upstream release."
    )
    parser.add_argument("--self-check", action="store_true", help="Run the fast fixture self-check.")
    parser.add_argument("--upstream", type=Path, help="Path to the unmodified upstream bundle directory.")
    parser.add_argument("--served", type=Path, help="Path to the served bundle directory.")
    parser.add_argument("--served-url", help="Base URL of a running proxy serving the upstream bundle.")
    parser.add_argument("--cookie", help="Optional Cookie header value when using --served-url.")
    parser.add_argument(
        "--overlay-css",
        type=Path,
        default=DEFAULT_OVERLAY_CSS,
        help="Path to ember/proxy/static/overlay.css.",
    )
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    if not args.overlay_css.is_file():
        print(f"error: overlay CSS not found: {args.overlay_css}", file=sys.stderr)
        return 2
    overlay_css = args.overlay_css.read_text(encoding="utf-8")

    if args.self_check or (args.upstream is None and args.served is None):
        return run_self_check(overlay_css)

    if args.upstream is None or not args.upstream.is_dir():
        print(f"error: --upstream directory not found: {args.upstream}", file=sys.stderr)
        return 2

    if args.served is not None:
        if not args.served.is_dir():
            print(f"error: --served directory not found: {args.served}", file=sys.stderr)
            return 2
        errors = diff_bundles(args.upstream, args.served, overlay_css)
    else:
        with tempfile.TemporaryDirectory() as tmp_str:
            served_tmp = Path(tmp_str) / "served"
            if args.served_url:
                fetch_served_from_url(args.upstream, served_tmp, args.served_url, args.cookie)
            else:
                if not proxy_upstream_in_process(args.upstream, served_tmp):
                    print(
                        "error: FastAPI is required to proxy --upstream in-process "
                        "(pass --served, --served-url, or run with ember/proxy/requirements.txt)",
                        file=sys.stderr,
                    )
                    return 2
            errors = diff_bundles(args.upstream, served_tmp, overlay_css)

    if errors:
        print(f"error: {len(errors)} bundle diff violation(s) found:", file=sys.stderr)
        for err in errors:
            print(err, file=sys.stderr)
        return 1

    count = len(collect_files(args.upstream))
    print(f"ok: served bundle matches upstream ({count} files checked)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
