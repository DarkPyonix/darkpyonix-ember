"""The workbench document gets detach.js next to overlay.js (SPEC FR-B1/B2).

Tests the pure rewrite in dpx/vscode/html_rewrite.py (no FastAPI needed) plus source-level
checks that the proxy serves `/__detach.js` and uses the rewrite.

    cd ember/proxy && python3 -m unittest
"""
from __future__ import annotations

import html as html_lib
import json
import re
import unittest
from pathlib import Path

from dpx.vscode import html_rewrite
from dpx.vscode.html_rewrite import add_configuration_defaults, workbench_inserts

PROXY = Path(__file__).resolve().parents[1]

CONFIG = {"folderUri": {"$mid": 1, "scheme": "vscode-remote", "path": "/p"},
          "productConfiguration": {"nameShort": "Code"}}
WORKBENCH = (
    "<!DOCTYPE html><html><head>"
    '<meta id="vscode-workbench-web-configuration" data-settings="'
    + html_lib.escape(json.dumps(CONFIG), quote=True)
    + '"><title>VS Code</title></head><body></body></html>'
)


def settings_of(doc: str) -> dict:
    m = re.search(r'id="vscode-workbench-web-configuration" data-settings="([^"]*)"', doc)
    assert m, "configuration meta missing"
    return json.loads(html_lib.unescape(m.group(1)))


class WorkbenchInsertsTest(unittest.TestCase):
    def test_detach_script_is_injected_with_overlay(self):
        doc, inserted = workbench_inserts(WORKBENCH, "123", detach=True)
        self.assertIn('<script src="/__detach.js?v=123" defer></script>', doc)
        self.assertIn('<script src="/__overlay.js?v=123" defer></script>', doc)
        self.assertEqual(inserted, ["viewport", "overlay.css", "overlay.js", "detach.js"])
        # Into <head>, after overlay.js (overlay first: detach.js does not depend on it, but the
        # order stays predictable for debugging).
        head = doc.split("</head>")[0]
        self.assertLess(head.index("/__overlay.js"), head.index("/__detach.js"))

    def test_detach_turns_off_vscode_drag_to_open_window(self):
        doc, _ = workbench_inserts(WORKBENCH, "1", detach=True)
        settings = settings_of(doc)
        self.assertIs(settings["configurationDefaults"]["workbench.editor.dragToOpenWindow"], False)
        # Everything serve-web put there survives.
        self.assertEqual(settings["folderUri"], CONFIG["folderUri"])
        self.assertEqual(settings["productConfiguration"], CONFIG["productConfiguration"])

    def test_detach_off_leaves_stock_behaviour(self):
        doc, inserted = workbench_inserts(WORKBENCH, "1", detach=False)
        self.assertNotIn("/__detach.js", doc)
        self.assertNotIn("detach.js", inserted)
        self.assertNotIn("configurationDefaults", settings_of(doc))

    def test_idempotent(self):
        once, _ = workbench_inserts(WORKBENCH, "1", detach=True)
        twice, inserted = workbench_inserts(once, "1", detach=True)
        self.assertEqual(once, twice)
        self.assertEqual(inserted, [])
        self.assertEqual(twice.count("/__detach.js"), 1)


class ThemeDefaultTest(unittest.TestCase):
    """The DarkPyonix theme is the default colour theme of the proxied workbench (FR-W6, #15)."""

    THEME_PACKAGE = PROXY.parents[1] / "extensions" / "vscode-darkpyonix-theme" / "package.json"

    def test_theme_defaults_are_added(self):
        doc, _ = workbench_inserts(WORKBENCH, "1", detach=False, theme=True)
        defaults = settings_of(doc)["configurationDefaults"]
        self.assertEqual(defaults["workbench.colorTheme"], "DarkPyonix Dark")
        self.assertEqual(defaults["workbench.preferredDarkColorTheme"], "DarkPyonix Dark")
        self.assertEqual(defaults["workbench.preferredLightColorTheme"], "DarkPyonix Light")
        self.assertNotIn("workbench.editor.dragToOpenWindow", defaults)

    def test_theme_and_detach_together(self):
        doc, _ = workbench_inserts(WORKBENCH, "1", detach=True, theme=True)
        defaults = settings_of(doc)["configurationDefaults"]
        self.assertIs(defaults["workbench.editor.dragToOpenWindow"], False)
        self.assertEqual(defaults["workbench.colorTheme"], "DarkPyonix Dark")
        again, _ = workbench_inserts(doc, "1", detach=True, theme=True)
        self.assertEqual(again, doc)

    def test_a_given_theme_default_wins(self):
        cfg = dict(CONFIG, configurationDefaults={"workbench.colorTheme": "Default Light Modern"})
        doc = WORKBENCH.replace(html_lib.escape(json.dumps(CONFIG), quote=True),
                                html_lib.escape(json.dumps(cfg), quote=True))
        out, _ = workbench_inserts(doc, "1", detach=False, theme=True)
        self.assertEqual(settings_of(out)["configurationDefaults"]["workbench.colorTheme"],
                         "Default Light Modern")

    def test_names_are_the_theme_labels(self):
        labels = {t["label"] for t in json.loads(self.THEME_PACKAGE.read_text(encoding="utf-8"))
                  ["contributes"]["themes"]}
        for value in html_rewrite.THEME_CONFIGURATION_DEFAULTS.values():
            self.assertIn(value, labels)

    def test_on_when_the_theme_is_a_default_extension(self):
        import os
        from unittest import mock
        from dpx.vscode import runtime
        with mock.patch.dict(os.environ, {"DPX_DEFAULT_EXTENSIONS": ""}):
            self.assertFalse(runtime.theme_is_default_extension())
        with mock.patch.dict(os.environ, {"DPX_DEFAULT_EXTENSIONS": "darkpyonix.vscode-darkpyonix-theme"}):
            self.assertTrue(runtime.theme_is_default_extension())
        with mock.patch.dict(os.environ, {"DPX_DEFAULT_EXTENSIONS": "/x/vscode-darkpyonix-theme-0.0.2.vsix"}):
            self.assertTrue(runtime.theme_is_default_extension())
        env = {k: v for k, v in os.environ.items() if k != "DPX_DEFAULT_EXTENSIONS"}
        with mock.patch.dict(os.environ, env, clear=True):
            self.assertTrue(runtime.theme_is_default_extension())


class ConfigurationDefaultsTest(unittest.TestCase):
    def test_existing_defaults_win(self):
        cfg = dict(CONFIG, configurationDefaults={"workbench.editor.dragToOpenWindow": True, "a": 1})
        doc = WORKBENCH.replace(html_lib.escape(json.dumps(CONFIG), quote=True),
                                html_lib.escape(json.dumps(cfg), quote=True))
        out = add_configuration_defaults(doc, html_rewrite.DETACH_CONFIGURATION_DEFAULTS)
        self.assertEqual(out, doc)

    def test_no_meta_or_bad_json_is_left_alone(self):
        plain = "<html><head></head></html>"
        self.assertEqual(add_configuration_defaults(plain, {"x": 1}), plain)
        bad = '<head><meta id="vscode-workbench-web-configuration" data-settings="{nope"></head>'
        self.assertEqual(add_configuration_defaults(bad, {"x": 1}), bad)


class ProxyWiringTest(unittest.TestCase):
    """Source-level: FastAPI is not needed to run these."""

    def test_route_and_asset_exist(self):
        main = (PROXY / "main.py").read_text(encoding="utf-8")
        self.assertIn('@app.get("/__detach.js")', main)
        self.assertIn('assets.script("detach.js")', main)
        self.assertTrue((PROXY / "static" / "detach.js").is_file())

    def test_inject_uses_the_rewrite(self):
        inject = (PROXY / "dpx" / "vscode" / "inject.py").read_text(encoding="utf-8")
        self.assertIn("html_rewrite.workbench_inserts(", inject)
        self.assertIn("detach=TAB_DETACH", inject)
        self.assertIn("theme=runtime.theme_is_default_extension()", inject)

    def test_asset_is_public_like_overlay(self):
        from dpx.auth.gate import is_public_path
        self.assertTrue(is_public_path("/__detach.js"))
        self.assertTrue(is_public_path("/__overlay.js"))


if __name__ == "__main__":
    unittest.main()
