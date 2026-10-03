"""Persistent terminal sessions of ember node, relayed to the browser (SPEC §P).

The terminal processes are owned by ember node on this computer, not by VS Code. The browser
side, meaning the companion extension in `companion/` (VS Code Web's web worker extension host) and
any other web client, reaches them through these same-origin routes, behind the session gate.
The node's bearer token stays in the proxy.

| File      | Role                                                                    |
|-----------|-------------------------------------------------------------------------|
| `node.py` | Pure helpers: finding the daemon, validating what the browser may send   |
| `api.py`  | The `/__terms/*` router: HTTP forwarding + the attach WebSocket relay    |

Design and the full attach protocol: `docs/design/TERMINALS.md` at the repository root.
"""
