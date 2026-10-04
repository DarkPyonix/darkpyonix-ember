"""The workbench document gets detach.js next to overlay.js (SPEC FR-B1/B2).

Tests the pure rewrite in dpx/vscode/html_rewrite.py (no FastAPI needed) plus source-level
checks that the proxy serves `/__detach.js` and uses the rewrite.

    cd web/proxy && python3 -m unittest
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

    def test_asset_is_public_like_overlay(self):
        from dpx.auth.gate import is_public_path
        self.assertTrue(is_public_path("/__detach.js"))
        self.assertTrue(is_public_path("/__overlay.js"))


if __name__ == "__main__":
    unittest.main()
