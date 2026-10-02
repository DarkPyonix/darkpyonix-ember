"""The auth gate — which paths are open without a session.

Every request bound for serve-web (the workspace editor, its assets and its WebSocket)
requires a valid session. This replaces VS Code's connection token.

The only public paths are the login page, the auth API, health, and the overlay assets.
Home (`/`) and `/__workspaces` need a session too, because home shows the machine's folder
paths and conversation previews — they used to be public, which meant all of it was visible
before signing in.

The middleware in the root `main.py` is what actually calls this.
"""
_PUBLIC_EXACT = {"/healthz", "/login", "/favicon.ico"}
_PUBLIC_PREFIX = ("/auth", "/__ext", "/__overlay", "/__kb")


def is_public_path(path: str) -> bool:
    """Is this path reachable without a session?"""
    return path in _PUBLIC_EXACT or path.startswith(_PUBLIC_PREFIX)
