"""Bundle diff check tests for the VS Code Web proxy wrapping layer.

Verifies that `.github/scripts/checks/check-vscode-bundle-diff.py` accepts a bundle
served with only the expected proxy injections and rejects any unallowlisted mutation
in JS, binary assets, HTML, or CSS.
"""
from __future__ import annotations

import html as html_lib
import importlib
import json
import os
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from dpx.vscode import html_rewrite

try:
    import fastapi  # noqa: F401
    from fastapi.testclient import TestClient
except ImportError:  # pragma: no cover - depends on environment
    TestClient = None

PROXY_DIR = Path(__file__).resolve().parents[1]
REPO_ROOT = PROXY_DIR.parents[1]
CHECK_SCRIPT = REPO_ROOT / ".github" / "scripts" / "checks" / "check-vscode-bundle-diff.py"
OVERLAY_CSS = (PROXY_DIR / "static" / "overlay.css").read_text(encoding="utf-8")
OVERLAY_MARKER = "\n\n/* --- Custom mobile overlay --- */\n"

SAMPLE_CONFIG = {
    "folderUri": {"$mid": 1, "scheme": "vscode-remote", "path": "/workspace"},
    "productConfiguration": {"nameShort": "Code - OSS"},
}
SAMPLE_WORKBENCH_HTML = (
    "<!DOCTYPE html>\n<html>\n<head>\n"
    '<meta charset="utf-8">\n'
    '<meta id="vscode-workbench-web-configuration" data-settings="'
    + html_lib.escape(json.dumps(SAMPLE_CONFIG), quote=True)
    + '">\n'
    "<title>Code - OSS</title>\n"
    "</head>\n<body><div id=\"workbench\"></div></body>\n</html>\n"
)
SAMPLE_WEBVIEW_HTML = (
    "<!DOCTYPE html>\n<html>\n<head>\n"
    '<meta charset="utf-8">\n'
    "</head>\n<body><script src=\"main.js\"></script></body>\n</html>\n"
)
SAMPLE_WORKER_IFRAME_HTML = (
    "<!DOCTYPE html>\n<html>\n<head><title>Worker</title></head>\n"
    "<body><script>/* stock worker host */</script></body>\n</html>\n"
)
SAMPLE_MAIN_CSS = ".monaco-workbench { color: #ccc; }\n"
SAMPLE_MAIN_JS = "define('vs/workbench/workbench.web.main', [], function () { return 1; });\n"
SAMPLE_PRODUCT_JSON = json.dumps({"nameShort": "Code - OSS", "version": "1.0.0"}, indent=2) + "\n"
SAMPLE_FONT_BYTES = b"\x00\x01\x00\x00codicon-fixture-font-bytes\xff\xfe"
SAMPLE_WASM_BYTES = b"\x00asm\x01\x00\x00\x00onig-fixture-wasm-bytes"
SAMPLE_PNG_BYTES = b"\x89PNG\r\n\x1a\nfixture-icon-bytes"


def write_bundle(root: Path, *, injected: bool = False, asset_version: str = "1700000000") -> None:
    """Creates a minimal VS Code Web bundle tree, optionally with clean proxy injections."""
    files: dict[str, bytes] = {
        "workbench.html": SAMPLE_WORKBENCH_HTML.encode("utf-8"),
        "out/vs/workbench/workbench.web.main.css": SAMPLE_MAIN_CSS.encode("utf-8"),
        "out/vs/workbench/workbench.web.main.js": SAMPLE_MAIN_JS.encode("utf-8"),
        "out/vs/workbench/contrib/webview/browser/pre/index.html": SAMPLE_WEBVIEW_HTML.encode("utf-8"),
        "out/vs/workbench/services/extensions/worker/webWorkerExtensionHostIframe.html": (
            SAMPLE_WORKER_IFRAME_HTML.encode("utf-8")
        ),
        "out/media/codicon.ttf": SAMPLE_FONT_BYTES,
        "node_modules/vscode-oniguruma/release/onig.wasm": SAMPLE_WASM_BYTES,
        "product.json": SAMPLE_PRODUCT_JSON.encode("utf-8"),
        "resources/code-icon.png": SAMPLE_PNG_BYTES,
    }
    if injected:
        wb_html, _ = html_rewrite.workbench_inserts(
            SAMPLE_WORKBENCH_HTML, asset_version, detach=True, theme=True
        )
        files["workbench.html"] = wb_html.encode("utf-8")
        files["out/vs/workbench/workbench.web.main.css"] = (
            SAMPLE_MAIN_CSS + OVERLAY_MARKER + OVERLAY_CSS
        ).encode("utf-8")
        files["out/vs/workbench/contrib/webview/browser/pre/index.html"] = (
            SAMPLE_WEBVIEW_HTML.replace(
                "</head>",
                f'<script src="/__kb.js?v={asset_version}" defer></script></head>',
                1,
            ).encode("utf-8")
        )
    for rel, data in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)


def run_check(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(CHECK_SCRIPT), *args],
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
    )


class BundleDiffCheckTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.base = Path(self.tmp.name)
        self.upstream = self.base / "upstream"
        self.served = self.base / "served"
        write_bundle(self.upstream, injected=False)
        write_bundle(self.served, injected=True)

    def tearDown(self):
        self.tmp.cleanup()

    def test_fr_w2_script_exists_and_is_executable(self):
        self.assertTrue(CHECK_SCRIPT.is_file(), f"missing check script: {CHECK_SCRIPT}")
        self.assertTrue(os.access(CHECK_SCRIPT, os.X_OK), f"script is not executable: {CHECK_SCRIPT}")

    def test_fr_w2_self_check_passes(self):
        res = run_check("--self-check")
        self.assertEqual(res.returncode, 0, f"stdout:\n{res.stdout}\nstderr:\n{res.stderr}")

    def test_fr_w2_clean_injection_passes(self):
        res = run_check("--upstream", str(self.upstream), "--served", str(self.served))
        self.assertEqual(res.returncode, 0, f"stdout:\n{res.stdout}\nstderr:\n{res.stderr}")

    def test_fr_w2_mutated_js_fails_with_unified_diff(self):
        js_path = self.served / "out/vs/workbench/workbench.web.main.js"
        js_path.write_text(SAMPLE_MAIN_JS + "// unexpected patch\n", encoding="utf-8")
        res = run_check("--upstream", str(self.upstream), "--served", str(self.served))
        self.assertNotEqual(res.returncode, 0, "mutated JS asset must fail the bundle diff check")
        combined = res.stdout + res.stderr
        self.assertIn("workbench.web.main.js", combined)
        self.assertIn("+// unexpected patch", combined)

    def test_fr_w2_mutated_binary_wasm_fails(self):
        wasm_path = self.served / "node_modules/vscode-oniguruma/release/onig.wasm"
        wasm_path.write_bytes(SAMPLE_WASM_BYTES + b"\x00")
        res = run_check("--upstream", str(self.upstream), "--served", str(self.served))
        self.assertNotEqual(res.returncode, 0, "mutated WASM binary must fail the bundle diff check")
        self.assertIn("onig.wasm", res.stdout + res.stderr)

    def test_fr_w2_unexpected_workbench_html_edit_fails(self):
        wb_path = self.served / "workbench.html"
        wb_path.write_text(
            wb_path.read_text(encoding="utf-8").replace("</body>", '<script src="/evil.js"></script></body>'),
            encoding="utf-8",
        )
        res = run_check("--upstream", str(self.upstream), "--served", str(self.served))
        self.assertNotEqual(res.returncode, 0, "unexpected workbench.html edit must fail")
        combined = res.stdout + res.stderr
        self.assertIn("workbench.html", combined)
        self.assertIn("/evil.js", combined)

    def test_fr_w2_unexpected_webview_html_edit_fails(self):
        wv_path = self.served / "out/vs/workbench/contrib/webview/browser/pre/index.html"
        wv_path.write_text(
            wv_path.read_text(encoding="utf-8").replace(
                "</head>", '<script src="/__overlay.js?v=1" defer></script></head>'
            ),
            encoding="utf-8",
        )
        res = run_check("--upstream", str(self.upstream), "--served", str(self.served))
        self.assertNotEqual(res.returncode, 0, "overlay.js inside webview host must fail")
        self.assertIn("webview/browser/pre/index.html", res.stdout + res.stderr)

    def test_fr_w2_internal_worker_iframe_mutation_fails(self):
        worker_path = (
            self.served / "out/vs/workbench/services/extensions/worker/webWorkerExtensionHostIframe.html"
        )
        worker_path.write_text(
            SAMPLE_WORKER_IFRAME_HTML.replace("</head>", '<script src="/__overlay.js"></script></head>'),
            encoding="utf-8",
        )
        res = run_check("--upstream", str(self.upstream), "--served", str(self.served))
        self.assertNotEqual(res.returncode, 0, "internal worker iframe HTML must stay unmodified")
        self.assertIn("webWorkerExtensionHostIframe.html", res.stdout + res.stderr)

    def test_fr_w2_unexpected_css_edit_fails(self):
        css_path = self.served / "out/vs/workbench/workbench.web.main.css"
        css_path.write_text(
            css_path.read_text(encoding="utf-8") + "\n.unexpected-rule { display: none; }\n",
            encoding="utf-8",
        )
        res = run_check("--upstream", str(self.upstream), "--served", str(self.served))
        self.assertNotEqual(res.returncode, 0, "unexpected edit in workbench.web.main.css must fail")
        combined = res.stdout + res.stderr
        self.assertIn("workbench.web.main.css", combined)
        self.assertIn(".unexpected-rule", combined)

    def test_fr_w2_extra_served_file_fails(self):
        extra = self.served / "out/vs/workbench/stray.js"
        extra.write_text("console.log('stray');\n", encoding="utf-8")
        res = run_check("--upstream", str(self.upstream), "--served", str(self.served))
        self.assertNotEqual(res.returncode, 0, "unexpected extra file in served bundle must fail")
        self.assertIn("stray.js", res.stdout + res.stderr)


@unittest.skipIf(
    TestClient is None,
    "FastAPI not installed (run under: uv run --with-requirements requirements.txt)",
)
class LiveProxyBundleDiffTest(unittest.TestCase):
    """Serves a fixture upstream bundle through the real ember/proxy app and diffs every response."""

    def test_fr_w2_live_proxy_bundle_matches_upstream_plus_allowlisted_injections(self):
        with tempfile.TemporaryDirectory() as tmp_str:
            base = Path(tmp_str)
            upstream_dir = base / "upstream"
            served_dir = base / "served"
            workspace_dir = base / "workspace"
            workspace_dir.mkdir()
            write_bundle(upstream_dir, injected=False)

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

                config.DB_FILE = base / "test.db"
                config.RECENT_FILE = base / "recent.json"
                main_mod = importlib.import_module("main")

                with TestClient(main_mod.app) as client:
                    login = client.post("/auth/login", json={"username": "u", "password": "p"})
                    self.assertEqual(login.status_code, 200, login.text)

                    for file_path in sorted(upstream_dir.rglob("*")):
                        if not file_path.is_file():
                            continue
                        rel = file_path.relative_to(upstream_dir).as_posix()
                        if rel == "workbench.html":
                            resp = client.get(
                                "/",
                                params={"folder": str(workspace_dir), "xmo": "embed"},
                                headers={"accept": "text/html"},
                            )
                        else:
                            headers = {"accept": "text/html"} if rel.endswith(".html") else {}
                            resp = client.get("/" + rel, headers=headers)
                        self.assertEqual(resp.status_code, 200, f"failed to fetch {rel}: {resp.status_code}")
                        out_path = served_dir / rel
                        out_path.parent.mkdir(parents=True, exist_ok=True)
                        out_path.write_bytes(resp.content)
            finally:
                server.shutdown()
                server.server_close()
                os.environ.clear()
                os.environ.update(saved_env)

            res = run_check("--upstream", str(upstream_dir), "--served", str(served_dir))
            self.assertEqual(res.returncode, 0, f"stdout:\n{res.stdout}\nstderr:\n{res.stderr}")


if __name__ == "__main__":
    unittest.main()
