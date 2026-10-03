"""The folder-root gate wired into main.py's middleware. Needs FastAPI (requirements.txt);
skipped when it is not installed, so plain `python3` still runs the rest of the suite."""
import importlib
import os
import sys
import tempfile
import unittest
from pathlib import Path

try:
    import fastapi  # noqa: F401
    from fastapi.testclient import TestClient
except ImportError:          # pragma: no cover - depends on the environment
    TestClient = None


@unittest.skipIf(TestClient is None, "FastAPI not installed (pip install -r requirements.txt)")
class MiddlewareRootsTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        base = Path(cls.tmp.name)
        cls.root = base / "work"
        cls.inside = cls.root / "proj"
        cls.outside = base / "elsewhere"
        cls.inside.mkdir(parents=True)
        cls.outside.mkdir()
        cls._env = dict(os.environ)
        os.environ.update({"DPX_FOLDER_ROOTS": str(cls.root), "DPX_USERNAME": "u",
                           "DPX_PASSWORD": "p", "XMO_UPSTREAM_PORT": "9"})
        for name in [m for m in sys.modules if m == "main" or m.startswith("dpx")]:
            del sys.modules[name]
        import dpx.config as config
        # Keep runtime state out of the repository.
        config.DB_FILE = base / "test.db"
        config.RECENT_FILE = base / "recent.json"
        cls.main = importlib.import_module("main")
        cls.client = TestClient(cls.main.app)
        r = cls.client.post("/auth/login", json={"username": "u", "password": "p"})
        assert r.status_code == 200, r.text

    @classmethod
    def tearDownClass(cls):
        os.environ.clear()
        os.environ.update(cls._env)
        cls.tmp.cleanup()

    def test_outside_folder_is_403(self):
        r = self.client.get("/", params={"folder": str(self.outside)},
                            headers={"accept": "text/html"})
        self.assertEqual(r.status_code, 403)

    def test_outside_workspace_is_403(self):
        r = self.client.get("/", params={"workspace": str(self.outside / "a.code-workspace")})
        self.assertEqual(r.status_code, 403)

    def test_inside_folder_gets_the_wrapper(self):
        r = self.client.get("/", params={"folder": str(self.inside)},
                            headers={"accept": "text/html"})
        self.assertEqual(r.status_code, 200)
        self.assertIn("<iframe", r.text.lower())


if __name__ == "__main__":
    unittest.main()
