"""The `/__terms` relay (dpx/terms/): endpoint discovery, what a browser may send, and HTTP
forwarding to a fake ember node.

    cd ember/proxy && python3 -m unittest tests.test_terms_relay

The pure helpers need nothing beyond the standard library. The router tests need the proxy's
requirements (fastapi, httpx) and are skipped without them.
"""
from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from dpx.terms import node

try:
    import httpx
    from fastapi import FastAPI
    from fastapi.testclient import TestClient
    HAVE_WEB = True
except ImportError:  # pragma: no cover - depends on the environment
    HAVE_WEB = False


class DiscoverTest(unittest.TestCase):
    def test_overrides_win_when_both_are_set(self):
        ep = node.discover("http://127.0.0.1:1", "tok", Path("/nonexistent"))
        self.assertEqual(ep, node.NodeEndpoint("http://127.0.0.1:1", "tok"))

    def test_endpoint_file_and_partial_override(self):
        with tempfile.TemporaryDirectory() as d:
            Path(d, "local.json").write_text(json.dumps({"url": "http://127.0.0.1:8741", "token": "t", "pid": 1}))
            self.assertEqual(node.discover("", "", Path(d)), node.NodeEndpoint("http://127.0.0.1:8741", "t"))
            self.assertEqual(node.discover("http://other:1", "", Path(d)).url, "http://other:1")

    def test_missing_or_broken_file_means_not_running(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertIsNone(node.discover("", "", Path(d)))
            Path(d, "local.json").write_text("{not json")
            self.assertIsNone(node.discover("", "", Path(d)))
            Path(d, "local.json").write_text(json.dumps({"url": "http://x"}))
            self.assertIsNone(node.discover("", "", Path(d)))

    def test_urls_and_auth_header(self):
        ep = node.NodeEndpoint("http://127.0.0.1:8741/", "s3cret")
        self.assertEqual(ep.http("/terms"), "http://127.0.0.1:8741/terms")
        self.assertEqual(ep.ws("/terms/a/attach"), "ws://127.0.0.1:8741/terms/a/attach")
        self.assertEqual(node.NodeEndpoint("https://h", "t").ws("/x"), "wss://h/x")
        self.assertEqual(ep.headers(), {"authorization": "Bearer s3cret"})


class SanitizeTest(unittest.TestCase):
    def test_create_defaults_and_drops_unknown_keys(self):
        out = node.sanitize_create({"cwd": None, "evil": 1, "size": {"rows": 30, "cols": 100}}, "/home/u")
        self.assertEqual(out, {"origin": "ide-vscode", "cwd": "/home/u", "size": {"rows": 30, "cols": 100}})

    def test_create_keeps_the_known_fields(self):
        body = {
            "program": {"argv": ["/bin/zsh", "-l"]}, "cwd": "/p", "env": {"A": "1"}, "env_clear": False,
            "origin": "ide-ember", "project": "/p", "title": "t", "key": "vscode:1", "tags": {"x": "y"},
        }
        self.assertEqual(node.sanitize_create(body, "/h"), body)

    def test_create_refuses_non_ide_origins_and_bad_shapes(self):
        for bad in (
            {"origin": "agent"}, {"origin": "user"}, {"size": {"rows": 0, "cols": 80}},
            {"size": {"rows": True, "cols": 80}}, {"env": {"A": 1}}, {"tags": []},
            {"program": "rm -rf /"}, {"program": {"argv": ["a"], "shell": "b"}}, {"title": 3},
            {"env_clear": "yes"},
        ):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                node.sanitize_create(bad, "/h")
        with self.assertRaises(ValueError):
            node.sanitize_create([1], "/h")

    def test_query_control_and_kill(self):
        self.assertEqual(node.sanitize_query({"project": "/p", "running": "true", "x": "1", "origin": ""}),
                         {"project": "/p", "running": "true"})
        for bad in ({"origin": "nope"}, {"running": "1"}):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                node.sanitize_query(bad)
        self.assertEqual(node.sanitize_control({"client": 3, "take": True}), {"client": 3, "take": True})
        for bad in ({"client": "3", "take": True}, {"client": 3}, {"client": True, "take": False}, None):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                node.sanitize_control(bad)
        self.assertEqual(node.sanitize_kill(None), {})
        self.assertEqual(node.sanitize_kill({"signal": 9}), {"signal": 9})
        with self.assertRaises(ValueError):
            node.sanitize_kill({"signal": 0})

    def test_ids_and_origin_check(self):
        self.assertTrue(node.valid_id("0123456789abcdef0123456789abcdef"))
        for bad in ("", "../x", "a/b", "a" * 65, "a b"):
            self.assertFalse(node.valid_id(bad), bad)
        self.assertTrue(node.same_origin(None, "h:8888"))
        self.assertTrue(node.same_origin("https://H:8888", "h:8888"))
        self.assertFalse(node.same_origin("https://evil.example", "h:8888"))
        self.assertFalse(node.same_origin("https://h:8888", None))


@unittest.skipUnless(HAVE_WEB, "fastapi/httpx not installed")
class RouterTest(unittest.TestCase):
    """The router alone (the session gate is main.py's middleware) against a fake node."""

    def setUp(self):
        from dpx.terms import api

        self.api = api
        self.calls = []

        def handler(request: httpx.Request) -> httpx.Response:
            body = json.loads(request.content) if request.content else None
            self.calls.append((request.method, request.url.path, dict(request.url.params), body,
                               request.headers.get("authorization")))
            if request.url.path == "/terms" and request.method == "POST":
                return httpx.Response(201, json={"created": True, "term": {"id": "abc"}})
            if request.url.path == "/terms/missing":
                return httpx.Response(404, json={"code": "not_found", "error": "no terminal session missing"})
            return httpx.Response(200, json=[])

        self._saved = (api.CLIENT, api.endpoint)
        api.CLIENT = httpx.AsyncClient(transport=httpx.MockTransport(handler))
        api.endpoint = lambda: node.NodeEndpoint("http://node.test", "tok")
        app = FastAPI()
        app.include_router(api.router)
        self.client = TestClient(app)

    def tearDown(self):
        self.api.CLIENT, self.api.endpoint = self._saved

    def test_list_forwards_filters_and_the_token(self):
        r = self.client.get("/__terms", params={"project": "/p", "running": "true", "junk": "1"})
        self.assertEqual(r.status_code, 200)
        self.assertEqual(self.calls, [("GET", "/terms", {"project": "/p", "running": "true"}, None, "Bearer tok")])

    def test_create_is_sanitized_and_status_passes_through(self):
        r = self.client.post("/__terms", json={"cwd": "/p", "origin": "ide-vscode", "key": "k", "extra": 1})
        self.assertEqual(r.status_code, 201)
        self.assertEqual(r.json()["term"]["id"], "abc")
        method, path, _, body, _ = self.calls[0]
        self.assertEqual((method, path), ("POST", "/terms"))
        self.assertEqual(body, {"cwd": "/p", "origin": "ide-vscode", "key": "k"})
        self.assertEqual(self.client.post("/__terms", json={"cwd": "/p", "origin": "agent"}).status_code, 400)
        self.assertEqual(len(self.calls), 1, "refused requests never reach the node")

    def test_errors_ids_control_and_kill(self):
        r = self.client.get("/__terms/missing")
        self.assertEqual(r.status_code, 404)
        self.assertEqual(r.json()["code"], "not_found")
        self.assertEqual(self.client.get("/__terms/a%2Fb").status_code in (400, 404), True)
        self.client.post("/__terms/abc/control", json={"client": 2, "take": False})
        self.client.post("/__terms/abc/kill")
        self.client.post("/__terms/abc/kill", json={"signal": 9})
        self.assertEqual([(c[0], c[1], c[3]) for c in self.calls[1:]], [
            ("POST", "/terms/abc/control", {"client": 2, "take": False}),
            ("POST", "/terms/abc/kill", {}),
            ("POST", "/terms/abc/kill", {"signal": 9}),
        ])

    def test_node_not_running(self):
        self.api.endpoint = lambda: None
        r = self.client.get("/__terms")
        self.assertEqual(r.status_code, 503)
        self.assertEqual(self.client.get("/__terms/_status").json()["available"], False)


if __name__ == "__main__":
    unittest.main()
