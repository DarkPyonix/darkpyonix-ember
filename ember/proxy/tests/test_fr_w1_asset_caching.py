"""Static workbench asset caching tests for the reverse proxy.

Run with:
    cd ember/proxy && uv run --with-requirements requirements.txt python -m unittest tests.test_fr_w1_asset_caching -v
"""
from __future__ import annotations

import importlib
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

try:
    import httpx
    from fastapi.testclient import TestClient
    HAVE_WEB = True
except ImportError:  # pragma: no cover - depends on the environment
    HAVE_WEB = False


@unittest.skipUnless(HAVE_WEB, "fastapi/httpx not installed (run under: uv run --with-requirements requirements.txt)")
class FrW1AssetCachingTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        base = Path(cls.tmp.name)
        cls.root = base / "work"
        cls.inside = cls.root / "proj"
        cls.inside.mkdir(parents=True)
        cls._env = dict(os.environ)
        os.environ.update({
            "DPX_FOLDER_ROOTS": str(cls.root),
            "DPX_USERNAME": "u",
            "DPX_PASSWORD": "p",
            "XMO_UPSTREAM_PORT": "9",
        })
        for name in [m for m in sys.modules if m == "main" or m.startswith("dpx")]:
            del sys.modules[name]
        import dpx.config as config
        config.DB_FILE = base / "test.db"
        config.RECENT_FILE = base / "recent.json"
        cls.main = importlib.import_module("main")
        cls.assets = importlib.import_module("dpx.assets")
        cls.proxy = importlib.import_module("dpx.vscode.proxy")
        cls.client = TestClient(cls.main.app)
        r = cls.client.post("/auth/login", json={"username": "u", "password": "p"})
        assert r.status_code == 200, r.text

    @classmethod
    def tearDownClass(cls):
        os.environ.clear()
        os.environ.update(cls._env)
        cls.tmp.cleanup()

    def test_fr_w1_static_script_and_stylesheet_have_cache_control_and_etag(self):
        for path in ("/__detach.js", "/__overlay.js", "/__overlay.css", "/__kb.js"):
            with self.subTest(path=path):
                r = self.client.get(path)
                self.assertEqual(r.status_code, 200)
                cc = r.headers.get("cache-control", "")
                self.assertIn("max-age=", cc)
                self.assertNotIn("no-store", cc)
                self.assertNotIn("no-cache", cc)
                etag = r.headers.get("etag", "")
                self.assertTrue(etag.startswith('"') and etag.endswith('"'), f"missing quoted ETag: {etag!r}")

    def test_fr_w1_static_assets_return_304_on_if_none_match(self):
        for path in ("/__detach.js", "/__overlay.css"):
            with self.subTest(path=path):
                first = self.client.get(path)
                self.assertEqual(first.status_code, 200)
                etag = first.headers["etag"]

                second = self.client.get(path, headers={"If-None-Match": etag})
                self.assertEqual(second.status_code, 304)
                self.assertEqual(second.content, b"")
                self.assertEqual(second.headers.get("etag"), etag)

                weak = self.client.get(path, headers={"If-None-Match": f"W/{etag}"})
                self.assertEqual(weak.status_code, 304)

                multi = self.client.get(path, headers={"If-None-Match": f'"other", {etag}'})
                self.assertEqual(multi.status_code, 304)

                miss = self.client.get(path, headers={"If-None-Match": '"different"'})
                self.assertEqual(miss.status_code, 200)
                self.assertGreater(len(miss.content), 0)

    def test_fr_w1_static_html_shell_has_etag_and_304_without_no_store(self):
        r = self.client.get("/", params={"folder": str(self.inside)}, headers={"accept": "text/html"})
        self.assertEqual(r.status_code, 200)
        cc = r.headers.get("cache-control", "")
        self.assertIn("no-cache", cc)
        self.assertNotIn("no-store", cc)
        etag = r.headers.get("etag", "")
        self.assertTrue(etag.startswith('"') and etag.endswith('"'), f"missing quoted ETag: {etag!r}")

        r304 = self.client.get(
            "/",
            params={"folder": str(self.inside)},
            headers={"accept": "text/html", "If-None-Match": etag},
        )
        self.assertEqual(r304.status_code, 304)
        self.assertEqual(r304.content, b"")

        err_resp = self.assets.page("upstream_down.html", status_code=502)
        self.assertIn("no-store", err_resp.headers.get("cache-control", ""))
        self.assertNotIn("etag", err_resp.headers)

    def test_fr_w1_workbench_main_css_is_cached_with_etag_and_304(self):
        css_path = "/stable-0123456789abcdef0123456789abcdef01234567/static/out/vs/workbench/workbench.web.main.css"
        upstream_body = "body { color: white; }"

        async def fake_fetch(request, url, *, follow_redirects=False):
            return httpx.Response(200, text=upstream_body, headers={"content-type": "text/css; charset=utf-8"})

        with mock.patch.object(self.proxy, "fetch", side_effect=fake_fetch):
            r = self.client.get(css_path)
            self.assertEqual(r.status_code, 200)
            self.assertIn(upstream_body, r.text)
            self.assertIn("Custom mobile overlay", r.text)
            cc = r.headers.get("cache-control", "")
            self.assertIn("max-age=", cc)
            self.assertNotIn("no-store", cc)
            self.assertNotIn("no-cache", cc)
            etag = r.headers.get("etag", "")
            self.assertTrue(etag.startswith('"') and etag.endswith('"'), f"missing quoted ETag: {etag!r}")

            r304 = self.client.get(css_path, headers={"If-None-Match": etag})
            self.assertEqual(r304.status_code, 304)
            self.assertEqual(r304.content, b"")
            self.assertEqual(r304.headers.get("etag"), etag)

        upstream_body_v2 = "body { color: black; }"

        async def fake_fetch_v2(request, url, *, follow_redirects=False):
            return httpx.Response(200, text=upstream_body_v2, headers={"content-type": "text/css; charset=utf-8"})

        with mock.patch.object(self.proxy, "fetch", side_effect=fake_fetch_v2):
            r2 = self.client.get(css_path, headers={"If-None-Match": etag})
            self.assertEqual(r2.status_code, 200)
            self.assertIn(upstream_body_v2, r2.text)
            self.assertNotEqual(r2.headers.get("etag"), etag)

    def test_fr_w1_workbench_top_html_stays_no_cache(self):
        doc = "<!DOCTYPE html><html><head><title>VS Code</title></head><body></body></html>"

        async def fake_fetch(request, url, *, follow_redirects=False):
            return httpx.Response(200, text=doc, headers={"content-type": "text/html; charset=utf-8"})

        with mock.patch.object(self.proxy, "fetch", side_effect=fake_fetch):
            r = self.client.get(
                "/",
                params={"folder": str(self.inside), "xmo": "embed"},
                headers={"accept": "text/html"},
            )
            self.assertEqual(r.status_code, 200)
            cc = r.headers.get("cache-control", "")
            self.assertIn("no-cache", cc)
            self.assertIn("/__overlay.js", r.text)

    def test_fr_w1_relayed_workbench_assets_get_immutable_or_max_age_cache_control(self):
        class _BytesStream(httpx.AsyncByteStream):
            def __init__(self, data: bytes):
                self._data = data

            async def __aiter__(self):
                yield self._data

            async def aclose(self):
                pass

        def handler(request: httpx.Request) -> httpx.Response:
            if request.url.path.endswith("with-upstream-cc.js"):
                return httpx.Response(
                    200,
                    stream=_BytesStream(b"console.log(1);"),
                    headers={"content-type": "application/javascript", "cache-control": "public, max-age=120"},
                )
            if request.url.path.endswith(".json") or request.url.path.startswith("/api/"):
                return httpx.Response(
                    200,
                    stream=_BytesStream(b'{"ok":true}'),
                    headers={"content-type": "application/json"},
                )
            return httpx.Response(
                200,
                stream=_BytesStream(b"asset-bytes"),
                headers={"content-type": "application/octet-stream"},
            )

        saved_client = self.proxy.CLIENT
        self.proxy.CLIENT = httpx.AsyncClient(transport=httpx.MockTransport(handler))
        try:
            r_js = self.client.get(
                "/stable-0123456789abcdef0123456789abcdef01234567/static/out/vs/workbench/workbench.web.main.js"
            )
            self.assertEqual(r_js.status_code, 200)
            self.assertEqual(r_js.headers.get("cache-control"), "public, max-age=31536000, immutable")

            r_font = self.client.get("/oss-abcdef1234567/static/out/media/codicon.ttf")
            self.assertEqual(r_font.status_code, 200)
            self.assertEqual(r_font.headers.get("cache-control"), "public, max-age=31536000, immutable")

            r_icon = self.client.get("/static/out/vs/workbench/icon.svg")
            self.assertEqual(r_icon.status_code, 200)
            self.assertEqual(r_icon.headers.get("cache-control"), "private, max-age=3600")

            r_keep = self.client.get(
                "/stable-0123456789abcdef0123456789abcdef01234567/static/with-upstream-cc.js"
            )
            self.assertEqual(r_keep.status_code, 200)
            self.assertEqual(r_keep.headers.get("cache-control"), "public, max-age=120")

            r_api = self.client.get("/api/data")
            self.assertEqual(r_api.status_code, 200)
            self.assertIsNone(r_api.headers.get("cache-control"))
        finally:
            self.proxy.CLIENT = saved_client


if __name__ == "__main__":
    unittest.main()
