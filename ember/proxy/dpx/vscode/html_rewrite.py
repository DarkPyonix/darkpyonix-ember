"""Pure string rewrites of the top-level workbench document.

Kept free of FastAPI so the rules can be unit-tested without the web stack
(`tests/test_inject.py`). `inject.workbench_html` fetches the document and calls these.
"""
from __future__ import annotations

import html as html_lib
import json
import logging
import re

logger = logging.getLogger("proxy")

# serve-web plants its IWorkbenchConstructionOptions as HTML-escaped JSON here.
_CONFIG_META = re.compile(
    r'(<meta\b[^>]*\bid="vscode-workbench-web-configuration"[^>]*\bdata-settings=")([^"]*)(")',
    re.IGNORECASE,
)

# Settings defaults applied when tab detach is on. A user's own settings still win.
#
# workbench.editor.dragToOpenWindow: VS Code's own "drop a tab outside the window → open a
# floating auxiliary editor window" (a `window.open` popup in the browser). With tab detach
# that would be the second of two windows for one gesture; with it off, VS Code only does
# that on Alt-drag, which detach.js leaves alone. detach.js also guards at run time in case
# a user setting turns it back on (see `vscodeNewWindowOp` there).
DETACH_CONFIGURATION_DEFAULTS = {"workbench.editor.dragToOpenWindow": False}


def add_configuration_defaults(html: str, defaults: dict) -> str:
    """Merges `defaults` into the `configurationDefaults` of the workbench web configuration
    meta. Keys already present are kept. Any surprise leaves the document unchanged."""
    m = _CONFIG_META.search(html)
    if not m:
        return html
    try:
        config = json.loads(html_lib.unescape(m.group(2)))
    except ValueError:
        logger.warning("workbench configuration meta is not JSON; configurationDefaults not added")
        return html
    if not isinstance(config, dict):
        return html
    current = config.get("configurationDefaults")
    if current is None:
        current = {}
    if not isinstance(current, dict):
        return html
    merged = dict(defaults)
    merged.update(current)
    if merged == current:
        return html
    config["configurationDefaults"] = merged
    encoded = html_lib.escape(json.dumps(config, separators=(",", ":")), quote=True)
    return html[: m.start(2)] + encoded + html[m.end(2):]


def workbench_inserts(html: str, asset_version: str, *, detach: bool) -> tuple[str, list[str]]:
    """Adds viewport, overlay CSS/JS and (when `detach`) detach.js to the workbench document.

    Returns the new document and the names of what was inserted, for the log line.
    """
    inserts: list[tuple[str, str]] = []
    if '<meta name="viewport"' not in html:
        inserts.append(("viewport",
                        '<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">'))
    if "/__overlay.css" not in html:
        inserts.append(("overlay.css", f'<link rel="stylesheet" href="/__overlay.css?v={asset_version}">'))
    if "/__overlay.js" not in html:
        inserts.append(("overlay.js", f'<script src="/__overlay.js?v={asset_version}" defer></script>'))
    if detach and "/__detach.js" not in html:
        inserts.append(("detach.js", f'<script src="/__detach.js?v={asset_version}" defer></script>'))
    if inserts:
        html = html.replace("<head>", "<head>" + "".join(tag for _, tag in inserts), 1)
    if detach:
        html = add_configuration_defaults(html, DETACH_CONFIGURATION_DEFAULTS)
    return html, [name for name, _ in inserts]
