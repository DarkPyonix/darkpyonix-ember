"""Relaying to the upstream (`code serve-web`) — HTTP and WebSocket.

This module does nothing but pass bytes through. Rewriting content (injecting the overlay
CSS/JS) lives in inject.py, and deciding which request goes where lives in the middleware
in main.py.
"""
import asyncio
import logging
from contextlib import suppress

import httpx
import websockets
from fastapi import Request, Response
from fastapi.responses import JSONResponse, StreamingResponse
from starlette.background import BackgroundTask
from starlette.websockets import WebSocket, WebSocketDisconnect

from dpx import assets
from dpx.config import UPSTREAM_BASE, UPSTREAM_HOST, UPSTREAM_PORT, UPSTREAM_WS_SCHEME

logger = logging.getLogger("proxy")

# One shared client: creating a new one per request loses the connection pool and
# keep-alive, and forces the whole body to be buffered.
CLIENT = httpx.AsyncClient(
    timeout=httpx.Timeout(30.0, connect=30.0),
    follow_redirects=True,
    limits=httpx.Limits(max_connections=64, max_keepalive_connections=20),
)

# Drop the re-framing headers (uvicorn re-chunks anyway).
_HOP_HEADERS = ("transfer-encoding", "content-length", "connection")


def upstream_url(path: str) -> str:
    return f"{UPSTREAM_BASE}{path if path.startswith('/') else '/' + path}"


def upstream_down(request: Request) -> Response:
    """Tells the user what to do when serve-web is not answering, instead of a blank 500."""
    if "text/html" in request.headers.get("accept", ""):
        return assets.page("upstream_down.html", status_code=502)
    return JSONResponse({"error": "upstream unavailable"}, status_code=502)


async def fetch(request: Request, url: str, *, follow_redirects: bool = False) -> httpx.Response | None:
    """A GET used when the whole body must be read in order to inject. None if upstream is down."""
    timeout = httpx.Timeout(30.0, connect=30.0)
    try:
        async with httpx.AsyncClient(timeout=timeout, follow_redirects=follow_redirects) as client:
            return await client.get(url, params=dict(request.query_params),
                                    headers={"host": request.headers.get("host", "")})
    except httpx.RequestError as exc:
        logger.info("upstream connection failed %s: %s", url, exc)
        return None


async def relay(request: Request, url: str) -> Response:
    """Passes one request through to the upstream, streaming.

    The client's Host header is **preserved on purpose**: serve-web writes that value into
    the workbench configuration (remoteAuthority, resourceUrlTemplate). If the upstream
    loopback host ends up there, the browser opens the remote WebSocket against
    127.0.0.1:<upstream>, which happens to work on the development machine and is
    unreachable from any other device (the tablet lockup).

    The body is **streamed** rather than buffered whole: large workbench assets start
    arriving immediately (the first tablet load was slow enough to time the remote
    connection out), and a long-lived stream cannot block the proxy.
    """
    body = await request.body()
    req = CLIENT.build_request(request.method, httpx.URL(url),
                               params=dict(request.query_params),
                               content=body, headers=dict(request.headers))
    try:
        resp = await CLIENT.send(req, stream=True)
    except httpx.RequestError as exc:
        logger.info("upstream connection failed %s: %s", url, exc)
        return upstream_down(request)

    # aiter_raw() hands the body bytes over untouched, so content-encoding stays valid.
    headers = dict(resp.headers)
    for h in _HOP_HEADERS:
        headers.pop(h, None)

    return StreamingResponse(
        resp.aiter_raw(),
        status_code=resp.status_code,
        headers=headers,
        media_type=resp.headers.get("content-type"),
        background=BackgroundTask(resp.aclose),
    )


async def relay_websocket(websocket: WebSocket, full_path: str) -> None:
    """Bidirectional relay for the WebSockets VS Code Web uses (the extension host, etc.).

    The caller must use @app.websocket — @app.websocket_route (plain starlette) does not
    inject path parameters into the handler, which crashes every connection.
    """
    subproto = websocket.headers.get("sec-websocket-protocol")
    await websocket.accept(subprotocol=subproto or None)

    url = f"{UPSTREAM_WS_SCHEME}://{UPSTREAM_HOST}:{UPSTREAM_PORT}/{full_path}"
    if websocket.url.query:
        url += f"?{websocket.url.query}"

    fwd_headers = {k: v for k, v in websocket.headers.items()
                   if k.lower() in {"cookie", "authorization"}}
    if subproto:
        fwd_headers["sec-websocket-protocol"] = subproto

    try:
        async with websockets.connect(
            url,
            extra_headers=fwd_headers,
            subprotocols=[subproto] if subproto else None,
            open_timeout=30,
            close_timeout=10,
            max_size=None,
        ) as upstream:
            async def client_to_upstream():
                try:
                    while True:
                        msg = await websocket.receive()
                        if msg.get("type") == "websocket.disconnect":
                            try:
                                await upstream.close()
                            finally:
                                break
                        if (data := msg.get("text")) is not None:
                            await upstream.send(data)
                        elif (data := msg.get("bytes")) is not None:
                            await upstream.send(data)
                except WebSocketDisconnect:
                    with suppress(Exception):
                        await upstream.close()

            async def upstream_to_client():
                try:
                    while True:
                        data = await upstream.recv()
                        if isinstance(data, (bytes, bytearray)):
                            await websocket.send_bytes(data)
                        else:
                            await websocket.send_text(data)
                except websockets.ConnectionClosed:
                    with suppress(Exception):
                        await websocket.close()

            await asyncio.gather(client_to_upstream(), upstream_to_client())
    except Exception as exc:
        logger.info("<< WS FAIL /%s: %s", full_path[:100], exc)
        with suppress(Exception):
            await websocket.close()
