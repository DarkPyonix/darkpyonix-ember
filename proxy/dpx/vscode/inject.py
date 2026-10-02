"""The rules for weaving our CSS/JS into upstream responses.

There are three injection points. What goes where is the whole of this file.

| Target                            | Injected                         | Why                                  |
|-----------------------------------|----------------------------------|--------------------------------------|
| The workbench main CSS            | static/overlay.css               | Mobile layout                        |
| The top-level workbench doc (`/`) | viewport + overlay.css + overlay.js | Bar, gestures, keyboard policy    |
| VS Code webview host frames       | static/webview-kb.js             | Keyboard policy for extension panels |

Injecting into **the top-level document only** is the load-bearing part. VS Code also
serves internal HTML documents (webWorkerExtensionHostIframe.html and friends), and putting
the overlay into one of those ran the script inside the extension host and froze Android
browsers completely.
"""
import logging
import re

from fastapi import Request, Response

from dpx import assets
from dpx.config import ASSET_VERSION
from dpx.vscode import proxy

logger = logging.getLogger("proxy")

# Matches the several variants of the VS Code workbench main CSS:
#   /.../out/vs/workbench/workbench.web.main(.nls)?.css
#   /.../out/vs/workbench/workbench.desktop.main(.nls)?.css
#   /.../out/vs/code/browser/workbench/workbench.css
MAIN_CSS_REGEX = re.compile(
    r"(?:^|/)out/vs/(?:workbench|code/browser/workbench)/workbench(?:\.(?:web|desktop)\.main(?:\.nls)?(?:\.min)?)?\.css(?:$|\?)"
)

# VS Code serves this document once per webview. The extension's real UI lives in a child
# iframe this document creates with `allow-same-origin`, and our script reaches that far.
WEBVIEW_HOST_HTML = "/webview/browser/pre/index.html"


def is_main_css(path: str) -> bool:
    return bool(MAIN_CSS_REGEX.search(path))


def is_webview_host(path: str) -> bool:
    return path.endswith(WEBVIEW_HOST_HTML)


async def main_css(request: Request, path: str) -> Response:
    """Fetches the workbench CSS and appends the overlay CSS to it."""
    url = proxy.upstream_url(path)
    resp = await proxy.fetch(request, url)
    if resp is None:
        return proxy.upstream_down(request)
    if resp.status_code == 200 and "text/css" in resp.headers.get("content-type", ""):
        overlay = assets.read("overlay.css")
        if overlay is None:
            logger.warning("static/overlay.css is missing; skipping injection")
            return await proxy.relay(request, url)
        logger.info("Applied mobile overlay CSS to: %s", path)
        return Response(content=resp.text + "\n\n/* --- Custom mobile overlay --- */\n" + overlay,
                        media_type="text/css", headers=assets.NO_CACHE)
    logger.info("CSS intercept matched but upstream response was not CSS or not 200 for: %s "
                "(status %s, content-type %s)", path, resp.status_code, resp.headers.get("content-type"))
    return await proxy.relay(request, url)


async def workbench_html(request: Request, path: str) -> Response:
    """Adds the viewport meta and the overlay link/script to the top-level workbench doc."""
    url = proxy.upstream_url(path)
    resp = await proxy.fetch(request, url, follow_redirects=True)
    if resp is None:
        return proxy.upstream_down(request)
    if not (resp.status_code == 200 and resp.headers.get("content-type", "").startswith("text/html")):
        return await proxy.relay(request, url)

    html = resp.text
    inserts = []
    if '<meta name="viewport"' not in html:
        inserts.append('<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">')
    if "/__overlay.css" not in html:
        inserts.append(f'<link rel="stylesheet" href="/__overlay.css?v={ASSET_VERSION}">')
    if "/__overlay.js" not in html:
        inserts.append(f'<script src="/__overlay.js?v={ASSET_VERSION}" defer></script>')
    if inserts:
        html = html.replace("<head>", "<head>" + "".join(inserts), 1)
        logger.info("Injected %s into HTML: %s", ", ".join(
            "viewport" if "viewport" in i else ("overlay.css" if "overlay.css" in i else "overlay.js")
            for i in inserts), path)
    return Response(content=html, media_type="text/html", headers=assets.NO_CACHE)


async def webview_kb(request: Request, path: str) -> Response:
    """Adds only the small keyboard policy to a webview host frame.

    The full workbench overlay is never added — that is exactly what ran inside the
    extension host frame and froze Android. The webview's CSP allows script-src 'self',
    so a same-origin tag needs no CSP surgery.
    """
    url = proxy.upstream_url(path)
    resp = await proxy.fetch(request, url)
    if resp is None:
        return proxy.upstream_down(request)
    ctype = resp.headers.get("content-type", "")
    if not (resp.status_code == 200 and ctype.startswith("text/html")) or "/__kb.js" in resp.text:
        return Response(content=resp.content, status_code=resp.status_code, media_type=ctype or None)
    html = resp.text.replace("</head>", f'<script src="/__kb.js?v={ASSET_VERSION}" defer></script></head>', 1)
    logger.info("Injected kb.js into webview host: %s", path)
    return Response(content=html, media_type="text/html", headers=assets.NO_CACHE)
