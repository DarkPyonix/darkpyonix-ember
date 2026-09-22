"""The DarkPyonix auth API (stage 1: username + password login, plus sessions).

Design notes
------------
- For now this app is **mounted** at `/auth` on the main.py gateway, exposing it on the same
  origin (port 8888), so the session cookie also reaches the proxy (/code and friends) and
  cross-origin/CORS is avoided.
- The code is still a **standalone FastAPI app** (`auth_app`), so it can later be split into
  its own process with main.py reverse-proxying `/auth/*` (or validating against a shared
  SQLite file).
- Storage is a **shared SQLite file** in the app folder (`darkpyonix.db`). The main.py gate
  calls `validate_session()` directly, in-process — the shared-SQLite coupling, which is the
  recommended starting point.
- Passwords are stored as stdlib `pbkdf2_hmac` hashes (no external dependency, 200k rounds).
- WARNING: **no default account is seeded.** An account is created only on a first run where
  both `DPX_USERNAME` and `DPX_PASSWORD` are given. How accounts should be issued is
  **undecided** — see docs/BACKGROUND.md §7-2.
- The `users` table supports multiple users, but **there is no per-user file isolation**. An
  account is less an identity than one of several entry passes: whoever logs in sees the files
  of the OS account that started the server.
"""
import os
import time
import hmac
import sqlite3
import secrets
import hashlib

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse

from dpx.config import DB_FILE

DB_PATH = str(DB_FILE)          # the repository root (dpx/config.py)
SESSION_COOKIE = "dpx_session"
SESSION_TTL = 60 * 60 * 24 * 14   # 14 days
PBKDF_ROUNDS = 200_000


def _conn():
    c = sqlite3.connect(DB_PATH, timeout=10)
    c.row_factory = sqlite3.Row
    return c


def _hash_pw(pw: str) -> str:
    salt = secrets.token_bytes(16)
    dk = hashlib.pbkdf2_hmac("sha256", pw.encode("utf-8"), salt, PBKDF_ROUNDS)
    return salt.hex() + "$" + dk.hex()


def _verify_pw(pw: str, stored: str) -> bool:
    try:
        salt_hex, dk_hex = stored.split("$", 1)
        dk = hashlib.pbkdf2_hmac("sha256", pw.encode("utf-8"), bytes.fromhex(salt_hex), PBKDF_ROUNDS)
        return hmac.compare_digest(dk.hex(), dk_hex)
    except Exception:
        return False


def init_db() -> None:
    """Creates the tables and seeds the first user (only when the env vars are given). Idempotent.

    ⚠️ **No default account is seeded.** This used to create `admin` / `darkpyonix`
    automatically when there were no users, but distributing that means **everybody who
    receives it starts on the same credentials.** One of them being exposed is enough.

    An account is now created only on a first run where **both** `DPX_USERNAME` and
    `DPX_PASSWORD` are given. Without them the server starts with no accounts and nobody can
    log in, which means no workspace opens. main.py logs that fact, and the fix, at startup.

    How accounts should be created is **still undecided** — see docs/BACKGROUND.md §7-2.
    """
    with _conn() as c:
        c.execute("CREATE TABLE IF NOT EXISTS users (username TEXT PRIMARY KEY, pw_hash TEXT NOT NULL, created REAL)")
        c.execute("CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, username TEXT, created REAL, expires REAL)")
        n = c.execute("SELECT COUNT(*) AS n FROM users").fetchone()["n"]
        if n:
            return
        username = (os.environ.get("DPX_USERNAME") or "").strip()
        pw = os.environ.get("DPX_PASSWORD") or ""
        if not username or not pw:
            return
        c.execute("INSERT INTO users (username, pw_hash, created) VALUES (?, ?, ?)",
                  (username, _hash_pw(pw), time.time()))


def _get_user(username: str):
    with _conn() as c:
        return c.execute("SELECT username, pw_hash FROM users WHERE username=?", (username,)).fetchone()


def create_session(username: str) -> str:
    sid = secrets.token_urlsafe(32)
    now = time.time()
    with _conn() as c:
        c.execute("INSERT INTO sessions (id, username, created, expires) VALUES (?, ?, ?, ?)",
                  (sid, username, now, now + SESSION_TTL))
        c.execute("DELETE FROM sessions WHERE expires < ?", (now,))   # sweep expired sessions
    return sid


def validate_session(sid: str) -> bool:
    """Called by the main.py gate. True if the session exists and has not expired."""
    if not sid:
        return False
    try:
        with _conn() as c:
            row = c.execute("SELECT expires FROM sessions WHERE id=?", (sid,)).fetchone()
        return bool(row) and row["expires"] > time.time()
    except Exception:
        return False


def session_user(sid: str):
    """The username attached to a session, or None if it is missing or expired."""
    if not sid:
        return None
    try:
        with _conn() as c:
            row = c.execute("SELECT username, expires FROM sessions WHERE id=?", (sid,)).fetchone()
        if row and row["expires"] > time.time():
            return row["username"]
    except Exception:
        pass
    return None


def delete_session(sid: str) -> None:
    if not sid:
        return
    with _conn() as c:
        c.execute("DELETE FROM sessions WHERE id=?", (sid,))


# --- User management ---------------------------------------------------------
def add_user(username: str, pw: str) -> bool:
    """Creates a new user. False if one already exists."""
    if not username or not pw:
        return False
    try:
        with _conn() as c:
            c.execute("INSERT INTO users (username, pw_hash, created) VALUES (?, ?, ?)",
                      (username, _hash_pw(pw), time.time()))
        return True
    except sqlite3.IntegrityError:
        return False


def list_users():
    with _conn() as c:
        rows = c.execute("SELECT username, created FROM users ORDER BY created").fetchall()
    return [{"username": r["username"], "created": r["created"]} for r in rows]


def user_count() -> int:
    with _conn() as c:
        return c.execute("SELECT COUNT(*) AS n FROM users").fetchone()["n"]


def delete_user(username: str) -> None:
    with _conn() as c:
        c.execute("DELETE FROM users WHERE username=?", (username,))
        c.execute("DELETE FROM sessions WHERE username=?", (username,))   # invalidate that user's sessions


def set_password(username: str, pw: str) -> bool:
    with _conn() as c:
        cur = c.execute("UPDATE users SET pw_hash=? WHERE username=?", (_hash_pw(pw), username))
        return cur.rowcount > 0


auth_app = FastAPI(title="DarkPyonix Auth")


@auth_app.on_event("startup")
async def _startup():
    init_db()


@auth_app.post("/login")
async def login(request: Request):
    """Verify username + password → create a session → set the session cookie."""
    try:
        data = await request.json()
    except Exception:
        data = {}
    username = ((data or {}).get("username") or "").strip()
    pw = (data or {}).get("password", "") or ""
    if not username:
        return JSONResponse({"ok": False, "error": "사용자 이름을 입력하세요"}, status_code=400)
    user = _get_user(username)
    if not user or not _verify_pw(pw, user["pw_hash"]):
        return JSONResponse({"ok": False, "error": "사용자 이름 또는 비밀번호가 올바르지 않습니다"}, status_code=401)
    sid = create_session(username)
    resp = JSONResponse({"ok": True, "username": username})
    # path='/' means the cookie reaches the whole 8888 origin (/code and friends), so the gate can read it.
    resp.set_cookie(SESSION_COOKIE, sid, max_age=SESSION_TTL,
                    httponly=True, samesite="lax", path="/")
    return resp


@auth_app.post("/logout")
async def logout(request: Request):
    delete_session(request.cookies.get(SESSION_COOKIE))
    resp = JSONResponse({"ok": True})
    resp.delete_cookie(SESSION_COOKIE, path="/")
    return resp


@auth_app.get("/me")
async def me(request: Request):
    username = session_user(request.cookies.get(SESSION_COOKIE))
    return {"authenticated": bool(username), "username": username}


# --- User management API (requires a login) ---------------------------------
# NOTE: right now *any* logged-in user can add or delete users. This can be narrowed to
#       admins later (for example a role column on the users table). The first cut simply
#       requires a session.
def _require_user(request: Request):
    return session_user(request.cookies.get(SESSION_COOKIE))


@auth_app.get("/users")
async def users_list(request: Request):
    if not _require_user(request):
        return JSONResponse({"ok": False, "error": "로그인이 필요합니다"}, status_code=401)
    return {"ok": True, "users": [u["username"] for u in list_users()]}


@auth_app.post("/users")
async def users_create(request: Request):
    if not _require_user(request):
        return JSONResponse({"ok": False, "error": "로그인이 필요합니다"}, status_code=401)
    try:
        data = await request.json()
    except Exception:
        data = {}
    username = ((data or {}).get("username") or "").strip()
    pw = (data or {}).get("password", "") or ""
    if not username or not pw:
        return JSONResponse({"ok": False, "error": "사용자 이름과 비밀번호가 필요합니다"}, status_code=400)
    if not add_user(username, pw):
        return JSONResponse({"ok": False, "error": "이미 존재하는 사용자 이름입니다"}, status_code=409)
    return {"ok": True, "username": username}


@auth_app.delete("/users/{username}")
async def users_delete(request: Request, username: str):
    me_name = _require_user(request)
    if not me_name:
        return JSONResponse({"ok": False, "error": "로그인이 필요합니다"}, status_code=401)
    if username == me_name:
        return JSONResponse({"ok": False, "error": "자기 자신은 삭제할 수 없습니다"}, status_code=400)
    if user_count() <= 1:
        return JSONResponse({"ok": False, "error": "마지막 사용자는 삭제할 수 없습니다"}, status_code=400)
    delete_user(username)
    return {"ok": True}


@auth_app.post("/change-password")
async def change_password(request: Request):
    me_name = _require_user(request)
    if not me_name:
        return JSONResponse({"ok": False, "error": "로그인이 필요합니다"}, status_code=401)
    try:
        data = await request.json()
    except Exception:
        data = {}
    old = (data or {}).get("old_password", "") or ""
    new = (data or {}).get("new_password", "") or ""
    if not new:
        return JSONResponse({"ok": False, "error": "새 비밀번호가 필요합니다"}, status_code=400)
    user = _get_user(me_name)
    if not user or not _verify_pw(old, user["pw_hash"]):
        return JSONResponse({"ok": False, "error": "현재 비밀번호가 올바르지 않습니다"}, status_code=401)
    set_password(me_name, new)
    return {"ok": True}
