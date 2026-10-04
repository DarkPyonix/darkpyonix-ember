"""Runtime detection and default-extension install (dpx/vscode/runtime.py). No real `code`."""
import json
import os
import subprocess
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

from dpx.vscode import runtime


def completed(stdout="", stderr="", code=0):
    return subprocess.CompletedProcess(args=[], returncode=code, stdout=stdout, stderr=stderr)


class DetectTest(unittest.TestCase):
    @mock.patch("dpx.vscode.runtime.shutil.which", return_value=None)
    def test_vsc_missing_gives_install_guide(self, _which):
        out = runtime.detect("vsc", "code")
        self.assertFalse(out["installed"])
        self.assertIn("not found", out["error"])
        self.assertTrue(out["install_guide"])
        self.assertTrue(any("code --version" in line for line in out["install_guide"]))

    @mock.patch("dpx.vscode.runtime.subprocess.run",
                return_value=completed("1.138.0\n7debcd0e\narm64\n"))
    @mock.patch("dpx.vscode.runtime.shutil.which", return_value="/usr/local/bin/code")
    def test_vsc_installed_parses_version(self, _which, run):
        out = runtime.detect("vsc", "code")
        self.assertTrue(out["installed"])
        self.assertEqual((out["version"], out["commit"], out["arch"]),
                         ("1.138.0", "7debcd0e", "arm64"))
        self.assertEqual(out["install_guide"], [])
        self.assertEqual(run.call_args.args[0], ["/usr/local/bin/code", "--version"])

    @mock.patch("dpx.vscode.runtime.subprocess.run", return_value=completed("", "boom", 1))
    @mock.patch("dpx.vscode.runtime.shutil.which", return_value="/usr/local/bin/code")
    def test_version_failure_is_not_installed(self, _which, _run):
        out = runtime.detect("vsc", "code")
        self.assertFalse(out["installed"])
        self.assertIn("boom", out["error"])

    @mock.patch("dpx.vscode.runtime.subprocess.run",
                side_effect=subprocess.TimeoutExpired("code", 15))
    @mock.patch("dpx.vscode.runtime.shutil.which", return_value="/usr/local/bin/code")
    def test_version_timeout_is_not_installed(self, _which, _run):
        self.assertFalse(runtime.detect("vsc", "code")["installed"])

    def test_ose_without_configured_server(self):
        with mock.patch.dict(os.environ, {}, clear=True):
            out = runtime.detect("ose")
        self.assertFalse(out["installed"])
        self.assertIn("DPX_OSE_SERVER", out["error"])
        self.assertTrue(any("DPX_OSE_SERVER" in line for line in out["install_guide"]))

    def test_default_runtime_is_ose(self):
        with mock.patch.dict(os.environ, {}, clear=True):
            self.assertEqual(runtime.default_runtime(), "ose")
        with mock.patch.dict(os.environ, {"DPX_RUNTIME": "VSC"}):
            self.assertEqual(runtime.default_runtime(), "vsc")
        with mock.patch.dict(os.environ, {"DPX_RUNTIME": "nonsense"}):
            self.assertEqual(runtime.default_runtime(), "ose")


class ServerCmdTest(unittest.TestCase):
    def test_vsc_serve_web(self):
        cmd = runtime.server_cmd("vsc", "/bin/code", "127.0.0.1", 9100, Path("/d"))
        self.assertEqual(cmd[:2], ["/bin/code", "serve-web"])
        self.assertIn("--without-connection-token", cmd)
        self.assertEqual(cmd[cmd.index("--port") + 1], "9100")
        self.assertEqual(cmd[cmd.index("--host") + 1], "127.0.0.1")
        self.assertEqual(cmd[cmd.index("--server-data-dir") + 1], "/d")

    def test_ose_default_template(self):
        with mock.patch.dict(os.environ, {}, clear=True):
            cmd = runtime.server_cmd("ose", "/ose/bin/code-server-oss", "127.0.0.1", 9101,
                                     Path("/data dir"))
        self.assertEqual(cmd[0], "/ose/bin/code-server-oss")
        self.assertEqual(cmd[cmd.index("--port") + 1], "9101")
        self.assertEqual(cmd[cmd.index("--server-data-dir") + 1], "/data dir")

    def test_ose_custom_template(self):
        cmd = runtime.server_cmd("ose", "/ose", "127.0.0.1", 7, Path("/x"),
                                 ose_args="serve --listen {host}:{port} --data {data_dir}")
        self.assertEqual(cmd, ["/ose", "serve", "--listen", "127.0.0.1:7", "--data", "/x"])


class ExtensionsTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name) / "extensions"

    def tearDown(self):
        self.tmp.cleanup()

    def _vsix(self, publisher="DarkPyonix", name="vscode-darkpyonix-theme"):
        path = Path(self.tmp.name) / "theme.vsix"
        with zipfile.ZipFile(path, "w") as z:
            z.writestr("extension/package.json",
                       json.dumps({"publisher": publisher, "name": name, "version": "0.1.0"}))
        return str(path)

    def test_installed_ids_strips_version_and_platform(self):
        (self.dir / "ms-python.python-2026.1.0-darwin-arm64").mkdir(parents=True)
        (self.dir / "darkpyonix.vscode-darkpyonix-theme-0.1.0").mkdir()
        (self.dir / ".obsolete").mkdir()
        self.assertEqual(runtime.installed_ids(self.dir),
                         {"ms-python.python", "darkpyonix.vscode-darkpyonix-theme"})

    def test_extension_id_from_vsix(self):
        self.assertEqual(runtime.extension_id(self._vsix()), "darkpyonix.vscode-darkpyonix-theme")

    @mock.patch("dpx.vscode.runtime.subprocess.run", return_value=completed("ok"))
    def test_install_then_skip(self, run):
        entries = ["DarkPyonix.vscode-darkpyonix-theme"]
        first = runtime.ensure_extensions(entries, self.dir, "/bin/code")
        self.assertEqual(first["installed"], ["darkpyonix.vscode-darkpyonix-theme"])
        self.assertEqual(run.call_args.args[0],
                         ["/bin/code", "--install-extension", entries[0],
                          "--extensions-dir", str(self.dir)])
        # Simulate what the real install leaves behind, then start again.
        (self.dir / "darkpyonix.vscode-darkpyonix-theme-0.1.0").mkdir()
        run.reset_mock()
        second = runtime.ensure_extensions(entries, self.dir, "/bin/code")
        self.assertEqual(second["skipped"], ["darkpyonix.vscode-darkpyonix-theme"])
        run.assert_not_called()

    @mock.patch("dpx.vscode.runtime.subprocess.run")
    def test_vsix_already_present_is_skipped(self, run):
        (self.dir / "darkpyonix.vscode-darkpyonix-theme-0.1.0").mkdir(parents=True)
        out = runtime.ensure_extensions([self._vsix()], self.dir, "/bin/code")
        self.assertEqual(out["skipped"], ["darkpyonix.vscode-darkpyonix-theme"])
        run.assert_not_called()

    @mock.patch("dpx.vscode.runtime.subprocess.run",
                return_value=completed("", "Extension 'x.y' not found.", 1))
    def test_failure_is_reported_not_raised(self, _run):
        out = runtime.ensure_extensions(["x.y"], self.dir, "/bin/code")
        self.assertEqual(out["installed"], [])
        self.assertEqual(out["failed"][0]["entry"], "x.y")
        self.assertIn("not found", out["failed"][0]["error"])

    def test_bad_vsix_is_reported(self):
        bad = Path(self.tmp.name) / "bad.vsix"
        bad.write_text("not a zip")
        out = runtime.ensure_extensions([str(bad)], self.dir, "/bin/code")
        self.assertEqual(len(out["failed"]), 1)

    def test_default_extensions_env(self):
        with mock.patch.dict(os.environ, {}, clear=True):
            self.assertEqual(runtime.default_extensions(), list(runtime.DEFAULT_EXTENSIONS))
        with mock.patch.dict(os.environ, {"DPX_DEFAULT_EXTENSIONS": ""}):
            self.assertEqual(runtime.default_extensions(), [])
        with mock.patch.dict(os.environ, {"DPX_DEFAULT_EXTENSIONS": os.pathsep.join(["a.b", "c.d"])}):
            self.assertEqual(runtime.default_extensions(), ["a.b", "c.d"])


if __name__ == "__main__":
    unittest.main()


class PrepareDataDirTest(unittest.TestCase):
    def test_ose_turns_off_signature_verification_and_keeps_other_settings(self):
        import json, tempfile
        from pathlib import Path
        from dpx.vscode import runtime
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "data" / "Machine" / "settings.json"
            p.parent.mkdir(parents=True)
            p.write_text('{"editor.fontSize": 14}')
            runtime.prepare_data_dir("ose", Path(d))
            self.assertEqual(json.loads(p.read_text()), {"editor.fontSize": 14, "extensions.verifySignature": False})
            u = Path(d) / "data" / "User" / "settings.json"
            self.assertEqual(json.loads(u.read_text()), {"extensions.verifySignature": False})

    def test_vsc_is_left_alone(self):
        import tempfile
        from pathlib import Path
        from dpx.vscode import runtime
        with tempfile.TemporaryDirectory() as d:
            runtime.prepare_data_dir("vsc", Path(d))
            self.assertFalse((Path(d) / "data").exists())
