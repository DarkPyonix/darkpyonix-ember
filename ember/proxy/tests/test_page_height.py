"""#1064: the proxy's own pages fill the window.

`html,body{min-height:100%}` left the body only as tall as its content, because a percentage
min-height resolves against an `html` with no height. In Ember's IDE window the rest showed the
colour the window paints on `<html>` before first paint (`ember-ide-webview` pre-paint), so the
login page covered only the top half. Each page now gives the body the viewport's height.
"""
from pathlib import Path

import pytest

STATIC = Path(__file__).resolve().parent.parent / "static"
PAGES = ["login.html", "workspace_login.html", "home.html", "hub_home.html"]


@pytest.mark.parametrize("page", PAGES)
def test_page_body_fills_the_viewport(page):
    css = (STATIC / page).read_text(encoding="utf-8")
    assert "body{min-height:100vh;min-height:100dvh;}" in css, page
    assert "html,body{margin:0;min-height:100%;}" not in css, page
