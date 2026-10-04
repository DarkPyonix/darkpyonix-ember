"""The launcher (dpx/serve.py): ports, the announced URL, commands, and the start sequence
with every subprocess mocked."""
import io
import json
import os
import socket
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock

from dpx import serve
from dpx.vscode import roots as roots_mod

INSTALLED = {"runtime": "vsc", "installed": True, "command": "code", "path": "/bin/code",
             "version": "1.138.0", "commit": "abc", "arch": "arm64", "error": None,
             "install_guide": []}


class FakeProc:
    def __init__(self, pid, polls):
        self.pid = pid
        self._polls = list(polls)
        self.returncode = None

    def poll(self):
        if self._polls:
            self.returncode = self._polls.pop(0)
        return self.returncode


class HelpersTest(unittest.TestCase):
    def test_free_port_is_bindable(self):
        port = serve.free_port()
        self.assertGreater(port, 0)
        with socket.socket() as s:
            s.bind((serve.LOOPBACK, port))

    def test_announce_url(self):
        self.assertEqual(serve.announce_url("127.0.0.1", 8123), "http://127.0.0.1:8123/")
        self.assertEqual(serve.announce_url("0.0.0.0", 8123), "http://127.0.0.1:8123/")
        self.assertEqual(serve.announce_url("::1", 8123), "http://[::1]:8123/")
        self.assertEqual(serve.announce_url("0.0.0.0", 8123, "https://mini.example.ts.net"),
                         "https://mini.example.ts.net/")

    def test_proxy_cmd_and_env(self):
        self.assertEqual(serve.proxy_cmd("/py", "0.0.0.0", 8888),
                         ["/py", "-m", "uvicorn", "main:app", "--host", "0.0.0.0", "--port", "8888"])
        env = serve.proxy_env({"KEEP": "1", "XMO_UPSTREAM_HOST": "evil"}, 9100, ["/a", "/b"],
                              "vsc", "/bin/code")
        self.assertEqual(env["KEEP"], "1")
        self.assertEqual(env["XMO_UPSTREAM_HOST"], "127.0.0.1")
        self.assertEqual(env["XMO_UPSTREAM_PORT"], "9100")
        self.assertEqual(env["DPX_FOLDER_ROOTS"].split(os.pathsep), ["/a", "/b"])
        self.assertEqual((env["DPX_RUNTIME"], env["DPX_CODE_BIN"]), ("vsc", "/bin/code"))
        ose = serve.proxy_env({}, 1, ["/a"], "ose", "/ose/server")
        self.assertEqual(ose["DPX_OSE_SERVER"], "/ose/server")

    def test_resolve_roots(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(serve.resolve_roots([d], None), [roots_mod.normalise(d)])
            self.assertEqual(serve.resolve_roots(None, d), [roots_mod.normalise(d)])
            with self.assertRaises(ValueError):
                serve.resolve_roots([os.path.join(d, "missing")], None)
        self.assertEqual(serve.resolve_roots(None, None), [])


class MainTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = self.tmp.name
        self.data = Path(self.tmp.name) / "data"

    def tearDown(self):
        self.tmp.cleanup()

    def run_main(self, argv):
        out, err = io.StringIO(), io.StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            code = serve.main(argv)
        return code, out.getvalue(), err.getvalue()

    def test_not_installed_prints_status_and_exits_3(self):
        missing = dict(INSTALLED, installed=False, path=None, error="not found",
                       install_guide=["install it"])
        with mock.patch.object(serve.runtime, "detect", return_value=missing):
            code, out, _ = self.run_main(["--runtime", "vsc", "--root", self.root])
        self.assertEqual(code, serve.EXIT_NOT_INSTALLED)
        self.assertEqual(json.loads(out)["install_guide"], ["install it"])

    def test_check_only(self):
        with mock.patch.object(serve.runtime, "detect", return_value=INSTALLED) as det:
            code, out, _ = self.run_main(["--runtime", "vsc", "--check"])
        self.assertEqual(code, 0)
        self.assertTrue(json.loads(out)["installed"])
        det.assert_called_once_with("vsc", None)

    def test_roots_required(self):
        with mock.patch.object(serve.runtime, "detect", return_value=INSTALLED), \
                mock.patch.dict(os.environ, {"DPX_FOLDER_ROOTS": ""}):
            code, _, err = self.run_main(["--runtime", "vsc", "--data-dir", str(self.data)])
        self.assertEqual(code, 2)
        self.assertIn("--root", err)

    def test_start_announce_and_stop(self):
        upstream = FakeProc(101, [None] * 50)
        proxy = FakeProc(102, [None, None, 0])        # dies after the ready line
        spawned = []

        def fake_spawn(cmd, **kw):
            spawned.append((cmd, kw))
            return upstream if len(spawned) == 1 else proxy

        announce = Path(self.tmp.name) / "ready.json"
        seen_announce = []
        stopped = []

        def fake_stop(p, grace=10.0):
            if p is proxy:
                seen_announce.append(announce.exists())
            stopped.append(p.pid)

        with mock.patch.object(serve.runtime, "detect", return_value=INSTALLED), \
                mock.patch.object(serve.runtime, "ensure_extensions",
                                  return_value={"installed": [], "skipped": [], "failed": []}) as ens, \
                mock.patch.object(serve, "free_port", side_effect=[9100, 8200]), \
                mock.patch.object(serve, "spawn", side_effect=fake_spawn), \
                mock.patch.object(serve, "wait_port", return_value=True), \
                mock.patch.object(serve, "wait_http_ok", return_value=True) as http_ok, \
                mock.patch.object(serve, "stop", side_effect=fake_stop), \
                mock.patch.object(serve.time, "sleep"), \
                mock.patch.object(serve.signal, "signal"):
            code, out, _ = self.run_main(["--runtime", "vsc", "--root", self.root,
                                          "--data-dir", str(self.data),
                                          "--extension", "a.b",
                                          "--announce-file", str(announce)])

        self.assertEqual(code, serve.EXIT_START_FAILED)       # the proxy died
        ready = json.loads(out.strip().splitlines()[-1])
        self.assertEqual(ready["event"], "ready")
        self.assertEqual(ready["url"], "http://127.0.0.1:8200/")
        self.assertEqual(ready["upstream_port"], 9100)
        self.assertEqual(ready["roots"], [roots_mod.normalise(self.root)])

        upstream_cmd, _ = spawned[0]
        self.assertEqual(upstream_cmd[:2], ["/bin/code", "serve-web"])
        self.assertEqual(upstream_cmd[upstream_cmd.index("--port") + 1], "9100")
        self.assertEqual(upstream_cmd[upstream_cmd.index("--host") + 1], "127.0.0.1")
        proxy_cmd, proxy_kw = spawned[1]
        self.assertEqual(proxy_cmd[-2:], ["--port", "8200"])
        self.assertEqual(proxy_kw["env"]["XMO_UPSTREAM_PORT"], "9100")
        self.assertEqual(proxy_kw["env"]["DPX_FOLDER_ROOTS"], roots_mod.normalise(self.root))
        http_ok.assert_called_once()
        self.assertEqual(http_ok.call_args.args[0], "http://127.0.0.1:8200/healthz")
        ens.assert_called_once_with(["a.b"], self.data / "extensions", "/bin/code")

        self.assertEqual(stopped, [102, 101])          # proxy first, then the server
        self.assertEqual(seen_announce, [True])        # written while running…
        self.assertFalse(announce.exists())            # …and removed on the way out

    def test_server_that_never_comes_up(self):
        upstream = FakeProc(101, [1])
        with mock.patch.object(serve.runtime, "detect", return_value=INSTALLED), \
                mock.patch.object(serve.runtime, "ensure_extensions", return_value={}), \
                mock.patch.object(serve, "free_port", return_value=9100), \
                mock.patch.object(serve, "spawn", return_value=upstream) as sp, \
                mock.patch.object(serve, "wait_port", return_value=False), \
                mock.patch.object(serve, "stop"), \
                mock.patch.object(serve.signal, "signal"):
            code, out, err = self.run_main(["--runtime", "vsc", "--root", self.root,
                                            "--data-dir", str(self.data)])
        self.assertEqual(code, serve.EXIT_START_FAILED)
        self.assertEqual(sp.call_count, 1)                 # the proxy was never started
        self.assertNotIn("ready", out)

    def test_default_data_dir_is_per_runtime(self):
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("DPX_SERVER_DATA_DIR", None)
            args = serve.parse_args(["--runtime", "vsc"])
        self.assertIsNone(args.data_dir)       # resolved in main() to DEFAULT_DATA_DIR / runtime


if __name__ == "__main__":
    unittest.main()
