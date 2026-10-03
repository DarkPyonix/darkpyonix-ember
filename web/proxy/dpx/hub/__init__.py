"""The hub, showing several machines on one home screen — optional.

| File           | Role                                                             |
|----------------|------------------------------------------------------------------|
| `server.py`    | The hub server app (`uvicorn dpx.hub.server:app`)                 |
| `connector.py` | The connector attaching this machine to the hub — started by the proxy or run standalone |

See docs/HUB.md at the repository root for details.
"""
