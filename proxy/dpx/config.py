"""Every environment variable, path and constant, in one place.

No setting that belongs here may be buried in the code elsewhere. Anything changed at
run time arrives as an `XMO_*` or `DPX_*` environment variable (see the README's
"Running it" section).
"""
import os
import time
from pathlib import Path

# --- Upstream: stock VS Code Web (`code serve-web`) -------------------------
# If it was started on a different port, XMO_UPSTREAM_PORT must say so (default 9092).
UPSTREAM_HOST = os.environ.get("XMO_UPSTREAM_HOST", "127.0.0.1")
UPSTREAM_PORT = int(os.environ.get("XMO_UPSTREAM_PORT", "9092"))
UPSTREAM_SCHEME = "http"
UPSTREAM_BASE = f"{UPSTREAM_SCHEME}://{UPSTREAM_HOST}:{UPSTREAM_PORT}"
UPSTREAM_WS_SCHEME = "ws" if UPSTREAM_SCHEME == "http" else "wss"

# --- Paths ------------------------------------------------------------------
# BASE_DIR = the repository root. This file lives in dpx/, so go up two levels.
# Runtime state files (the database, the recent list, hub state) all live at the root:
# inside the package they would mix code with data, and the data would follow the
# package around every time it moved.
BASE_DIR = Path(__file__).resolve().parent.parent
STATIC_DIR = BASE_DIR / "static"
RECENT_FILE = BASE_DIR / "_xmo_recent.json"
DB_FILE = BASE_DIR / "darkpyonix.db"
HUB_STATE_FILE = BASE_DIR / "_dpx_hub_state.json"
MACHINE_ID_FILE = BASE_DIR / "_dpx_machine.json"

# --- Cache busting for injected assets --------------------------------------
# Regenerated every time the process starts (uvicorn --reload restarts on each edit),
# so CSS/JS changes show up immediately and media queries are re-evaluated on
# rotation and resize.
ASSET_VERSION = str(int(time.time()))

# --- The extension gate -----------------------------------------------------
# The companion VS Code extension sends a heartbeat roughly every 10 seconds; if none
# arrives within this window, the gate closes.
EXT_TTL_SECONDS = 30.0

# How many recent workspaces to remember.
RECENT_LIMIT = 20
