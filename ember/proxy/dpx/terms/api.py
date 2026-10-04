"""`/__terms/*`: ember node's persistent terminal sessions, relayed for the browser.

| Method | Path                          | ember node                          |
|--------|-------------------------------|-------------------------------------|
| GET    | `/__terms/_status`            | (is the daemon reachable?)          |
| GET    | `/__terms?project=&origin=&running=` | `GET /v1/terms`              |
| POST   | `/__terms`                    | `POST /v1/terms` (IDE origins only) |
| GET    | `/__terms/{id}`               | `GET /v1/terms/{id}`                |
| DELETE | `/__terms/{id}`               | `DELETE /v1/terms/{id}`             |
| GET    | `/__terms/{id}/snapshot`      | `GET /v1/terms/{id}/snapshot`       |
| POST   | `/__terms/{id}/control`       | `POST /v1/terms/{id}/control`       |
| POST   | `/__terms/{id}/kill`          | `POST /v1/terms/{id}/kill`          |
| WS     | `/__terms/{id}/attach`        | `WS /v1/terms/{id}/attach` (frames passed through unchanged) |

HTTP routes are behind the session gate of `main.py`'s middleware (they are not public). The
WebSocket route is not seen by that HTTP middleware, so it checks the session cookie and the
`Origin` itself. Closing the browser's socket only detaches; the session lives on in the node.
"""
from __future__ import annotations

import asyncio
import json
import logging
from contextlib import suppress
from pathlib import Path

import httpx
import websockets
from fastapi import APIRouter, Request, Response
from fastapi.responses import JSONResponse
from starlette.websockets import WebSocket, WebSocketDisconnect

from dpx import config
from dpx.terms import node

logger = logging.getLogger("proxy")

router = APIRouter()

CLIENT = httpx.AsyncClient(timeout=httpx.Timeout(15.0, connect=5.0))


def endpoint() -> node.NodeEndpoint | None:
    # Re-read every time: the node writes a new local.json when it restarts (maybe on a new port).
    return node.discover(config.EMBER_NODE_URL, config.EMBER_NODE_TOKEN, config.EMBER_NODE_STATE_DIR)


def _error(status: int, message: str) -> JSONResponse:
    return JSONResponse({"error": message}, status_code=status)


async def _forward(method: str, path: str, *, params: dict | None = None, body: dict | None = None) -> Response:
    ep = endpoint()
    if ep is None:
        return _error(503, "ember node is not running on this computer")
    try:
        r = await CLIENT.request(method, ep.http(path), params=params, json=body, headers=ep.headers())
    except httpx.RequestError as exc:
        logger.info("ember node unreachable %s: %s", path, exc)
        return _error(502, f"ember node unreachable: {exc}")
    return Response(content=r.content, status_code=r.status_code,
                    media_type=r.headers.get("content-type", "application/json"))


async def _json_body(request: Request):
    raw = await request.body()
    if not raw:
        return None
    try:
        return json.loads(raw)
    except ValueError:
        raise ValueError("body is not JSON")


@router.get("/__terms/_status")
async def terms_status():
    ep = endpoint()
    if ep is None:
        return {"available": False, "reason": "ember node endpoint not found"}
    try:
        r = await CLIENT.get(ep.http("/v1/health"))
        return {"available": r.status_code == 200}
    except httpx.RequestError as exc:
        return {"available": False, "reason": str(exc)}


@router.get("/__terms")
async def list_terms(request: Request):
    try:
        q = node.sanitize_query(request.query_params)
    except ValueError as exc:
        return _error(400, str(exc))
    return await _forward("GET", "/v1/terms", params=q)


@router.post("/__terms")
async def create_term(request: Request):
    try:
        body = node.sanitize_create(await _json_body(request), str(Path.home()))
    except ValueError as exc:
        return _error(400, str(exc))
    return await _forward("POST", "/v1/terms", body=body)


@router.get("/__terms/{term_id}")
async def get_term(term_id: str):
    if not node.valid_id(term_id):
        return _error(400, "bad session id")
    return await _forward("GET", f"/v1/terms/{term_id}")


@router.delete("/__terms/{term_id}")
async def remove_term(term_id: str):
    if not node.valid_id(term_id):
        return _error(400, "bad session id")
    return await _forward("DELETE", f"/v1/terms/{term_id}")


@router.get("/__terms/{term_id}/snapshot")
async def term_snapshot(term_id: str):
    if not node.valid_id(term_id):
        return _error(400, "bad session id")
    return await _forward("GET", f"/v1/terms/{term_id}/snapshot")


@router.post("/__terms/{term_id}/control")
async def term_control(term_id: str, request: Request):
    if not node.valid_id(term_id):
        return _error(400, "bad session id")
    try:
        body = node.sanitize_control(await _json_body(request))
    except ValueError as exc:
        return _error(400, str(exc))
    return await _forward("POST", f"/v1/terms/{term_id}/control", body=body)


@router.post("/__terms/{term_id}/kill")
async def kill_term(term_id: str, request: Request):
    if not node.valid_id(term_id):
        return _error(400, "bad session id")
    try:
        body = node.sanitize_kill(await _json_body(request))
    except ValueError as exc:
        return _error(400, str(exc))
    return await _forward("POST", f"/v1/terms/{term_id}/kill", body=body)


def _ws_authorized(websocket: WebSocket) -> bool:
    # Imported here so the module (and its tests) do not open the user database on import.
    from dpx.auth.api import SESSION_COOKIE, validate_session
    if not node.same_origin(websocket.headers.get("origin"), websocket.headers.get("host")):
        return False
    return bool(validate_session(websocket.cookies.get(SESSION_COOKIE)))


@router.websocket("/__terms/{term_id}/attach")
async def attach_term(websocket: WebSocket, term_id: str):
    """Pass the attach protocol through unchanged: the browser's `TermHello` and `TermInput`
    frames go to the node, its `TermEvent` frames come back. Either side closing ends both;
    for the node that is a detach, never a kill."""
    if not node.valid_id(term_id) or not _ws_authorized(websocket):
        await websocket.close(code=1008)
        return
    ep = endpoint()
    await websocket.accept()
    if ep is None:
        await websocket.send_text(json.dumps({"type": "error", "message": "ember node is not running on this computer"}))
        await websocket.close(code=1011)
        return
    try:
        async with websockets.connect(
            ep.ws(f"/v1/terms/{term_id}/attach"),
            extra_headers=ep.headers(),   # websockets 12.x name (see requirements.txt)
            open_timeout=10,
            close_timeout=5,
            max_size=None,
        ) as upstream:
            async def browser_to_node():
                try:
                    while True:
                        msg = await websocket.receive()
                        if msg.get("type") == "websocket.disconnect":
                            break
                        if (text := msg.get("text")) is not None:
                            await upstream.send(text)
                finally:
                    with suppress(Exception):
                        await upstream.close()

            async def node_to_browser():
                try:
                    async for data in upstream:
                        if isinstance(data, (bytes, bytearray)):
                            await websocket.send_bytes(bytes(data))
                        else:
                            await websocket.send_text(data)
                except websockets.ConnectionClosed:
                    pass
                finally:
                    with suppress(Exception):
                        await websocket.close()

            await asyncio.gather(browser_to_node(), node_to_browser())
    except WebSocketDisconnect:
        pass
    except Exception as exc:
        logger.info("terminal attach relay failed %s: %s", term_id, exc)
        with suppress(Exception):
            await websocket.send_text(json.dumps({"type": "error", "message": f"ember node unreachable: {exc}"}))
        with suppress(Exception):
            await websocket.close(code=1011)
