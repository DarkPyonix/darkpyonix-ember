"""Authentication — the login app and the gate.

| File      | Role                                                              |
|-----------|-------------------------------------------------------------------|
| `api.py`  | A standalone FastAPI app mounted at `/auth` — users, sessions, SQLite |
| `gate.py` | Decides which paths are open without a session                     |

The gate replaces VS Code's connection token: every request bound for serve-web
must carry a valid session.
"""
from .gate import is_public_path
