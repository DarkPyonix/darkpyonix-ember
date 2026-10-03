"""Folder-root restriction (dpx/vscode/roots.py)."""
import os
import tempfile
import unittest
from pathlib import Path

from dpx.vscode import roots


class FolderRootsTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name)
        self.root = base / "work"
        self.inside = self.root / "proj"
        self.outside = base / "elsewhere"
        for d in (self.inside, self.outside):
            d.mkdir(parents=True)
        self.roots = roots.parse_roots(str(self.root))

    def tearDown(self):
        self.tmp.cleanup()

    def test_no_roots_means_no_restriction(self):
        self.assertTrue(roots.is_allowed(str(self.outside), []))
        self.assertIsNone(roots.refused_param({"folder": "/etc"}, []))

    def test_root_and_descendants_allowed(self):
        self.assertTrue(roots.is_allowed(str(self.root), self.roots))
        self.assertTrue(roots.is_allowed(str(self.inside), self.roots))
        self.assertTrue(roots.is_allowed(str(self.inside / "not-yet-created"), self.roots))

    def test_outside_refused(self):
        self.assertFalse(roots.is_allowed(str(self.outside), self.roots))
        self.assertFalse(roots.is_allowed("/", self.roots))

    def test_sibling_with_common_prefix_refused(self):
        sibling = Path(str(self.root) + "-evil")
        sibling.mkdir()
        self.assertFalse(roots.is_allowed(str(sibling), self.roots))

    def test_dotdot_escape_refused(self):
        self.assertFalse(roots.is_allowed(f"{self.inside}/../../elsewhere", self.roots))

    def test_symlink_out_of_root_refused(self):
        link = self.root / "link"
        os.symlink(self.outside, link)
        self.assertFalse(roots.is_allowed(str(link), self.roots))

    def test_relative_empty_and_foreign_uris_refused(self):
        for bad in ("", "  ", "proj", "vscode-remote://ssh-remote+x/home", None):
            self.assertFalse(roots.is_allowed(bad, self.roots), bad)

    def test_file_uri_checked_by_path(self):
        self.assertTrue(roots.is_allowed("file://" + str(self.inside).replace(" ", "%20"),
                                         self.roots))
        self.assertFalse(roots.is_allowed("file://" + str(self.outside), self.roots))

    def test_refused_param_checks_folder_and_workspace(self):
        ok = {"folder": str(self.inside)}
        self.assertIsNone(roots.refused_param(ok, self.roots))
        self.assertEqual(roots.refused_param({"folder": str(self.outside)}, self.roots), "folder")
        ws = {"folder": str(self.inside), "workspace": str(self.outside / "x.code-workspace")}
        self.assertEqual(roots.refused_param(ws, self.roots), "workspace")
        self.assertIsNone(roots.refused_param({"xmo": "embed"}, self.roots))

    def test_parse_roots_splits_on_pathsep_and_skips_empty(self):
        value = os.pathsep.join([str(self.root), "", str(self.outside)])
        self.assertEqual(roots.parse_roots(value),
                         [roots.normalise(str(self.root)), roots.normalise(str(self.outside))])
        self.assertEqual(roots.parse_roots(None), [])


if __name__ == "__main__":
    unittest.main()
