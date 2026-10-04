"""Everything on the VS Code Web (`code serve-web`) side.

| File           | Role                                                          |
|----------------|---------------------------------------------------------------|
| `proxy.py`     | Upstream relay: HTTP streaming, WebSocket, the down page      |
| `inject.py`    | The rules for weaving the overlay CSS/JS into upstream responses |
| `html_rewrite.py` | Pure rewrites of the workbench document (inserts, settings defaults) |
| `extension.py` | The companion VS Code extension's heartbeat gate + `/__ext/*`  |
| `roots.py`     | Folder roots: which `?folder=` / `?workspace=` may be opened  |
| `runtime.py`   | OSE / VSC runtime detection, server command, default extensions |

`proxy` passes bytes through untouched; only `inject` rewrites content. What goes
to which side is decided by the middleware in the root `main.py`.
"""
