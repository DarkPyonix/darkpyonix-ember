# (Optional) The hub — when you use more than one machine

**Skip this document if you only use one machine.** The default home screen
(`../AGENTS.md`) already shows folders → conversations → transcripts. The hub is a
separate server (`dpx/hub/`) you only run when you want **several** machines — a home
desktop, a school laptop — on a single home screen. Shared material, such as how to add an
adapter, is in `../AGENTS.md`.

```
   phone / tablet / desktop browser
            │  HTTPS (log in once)
            ▼
      ┌───────────┐  ws (outbound)   ┌──────────────┐
      │    hub    │◀─────────────────│ home-desktop │ hub/connector
      │ hub/server │◀─────────────────│ school-laptop│ hub/connector
      └───────────┘                  └──────────────┘
       home screen              each machine's main.py (the VS Code proxy) is untouched
```

* **The connector dials out to the hub**, so no machine needs port forwarding or a
  certificate of its own.
* Every 20 seconds the connector pushes a **snapshot** (workspaces + recent conversation
  summaries), so the first home screen renders immediately without waking any machine.
* Only large things, such as a full transcript, trigger an RPC to that machine on demand
  (which requires it to be powered on).
* If a machine is off, its last snapshot remains, so the card does not disappear — it is
  shown as offline.

## Screen structure

```
machine list  →  workspace list      →  conversation list     →  full transcript
   🖥             📁 folder, convo count   💬 title, preview      bubbles + [Resume in VS Code]
```

## Running it

### 1. The hub (one always-on machine — a home server, a VPS, or the desktop)

```bat
set DPX_HUB_TOKEN=a-long-string-nobody-else-knows
uvicorn dpx.hub.server:app --host 0.0.0.0 --port 8900
```

To serve it over HTTPS, pass a certificate to uvicorn's `--ssl-keyfile` / `--ssl-certfile`.
**How that certificate should be obtained is still undecided** — see [BACKGROUND.md](BACKGROUND.md).

Accounts come from the existing `darkpyonix.db` (the same users and passwords as the proxy).

### 2. The connector, on each machine

```bat
set DPX_HUB_URL=wss://hub-address:8900/hub/connect
set DPX_HUB_TOKEN=the-same-token-as-the-hub
set DPX_MACHINE_NAME=home-desktop
set DPX_PUBLIC_URL=https://my-pc.example.com:8888
python -m dpx.hub.connector
```

If `DPX_HUB_URL` is set, **main.py starts the connector along with itself**
(`_start_hub_connector`), in which case running `python -m dpx.hub.connector` separately is
unnecessary.

### 3. Open the hub address on the phone → log in → machine cards

## Environment variables

| Variable | Used by | Description |
|---|---|---|
| `DPX_HUB_TOKEN` | hub, connector | Shared token authenticating connectors. **If empty, every connector is rejected** |
| `DPX_HUB_URL` | connector | The hub's WebSocket address (`wss://.../hub/connect`) |
| `DPX_PUBLIC_URL` | connector | This machine's DarkPyonix address. Without it, "Open in VS Code" does not work |
| `DPX_MACHINE_NAME` | connector | The name shown on the home screen (default: hostname) |
| `DPX_MACHINE_ID` | connector | Stable identifier (default: generated into `_dpx_machine.json`) |
| `DPX_HOME` | connector | Home directory to look for session files in (default: the user's home) |
| `DPX_SNAPSHOT_INTERVAL` | connector | Snapshot interval in seconds (default 20) |
| `DPX_OFFLINE_AFTER` | hub | Mark a machine offline after this many seconds of silence (default 75) |

## Supported agents

| Adapter | Reads from | Status |
|---|---|---|
| `claude-code` | `~/.claude/projects/*/*.jsonl` | Supported (titles, full transcripts, resume via `claude --resume`) |
| `codex` | `~/.codex/sessions/**/*.jsonl` | Experimental — silently skips anything in an unexpected format |

### Adding a new agent

Write a module implementing `AgentAdapter` from `dpx/agents/base.py` and add one line to
`_CANDIDATES` in `dpx/agents/__init__.py`. That is all — the connector, the hub and the home
screen need no changes.

```python
class MyAgentAdapter(AgentAdapter):
    id = "my-agent"; name = "My Agent"
    def available(self): ...        # does this machine have any history?
    def conversations(self, folder=None): ...   # the list (title, time, preview)
    def transcript(self, cid, limit=300): ...   # the full transcript
    def resume(self, cid): ...      # {"kind":"terminal","command":..., "cwd":...}
```

`workspaces()` already has a default implementation that groups `conversations()` by folder.

## What "Resume" actually does

There is no way for the browser to type a command into a terminal inside VS Code on the
user's behalf. So:

1. Home asks the hub for the resume command (`claude --resume <id>`) and **copies it to the
   clipboard**
2. Navigates to that machine's `/?folder=...&agent=...&cid=...`
3. The frame wrapper in main.py opens the workspace and **opens the agent panel
   automatically**
4. Pasting into the terminal resumes that conversation

## Known limitations

* A full transcript can only be read while that machine is **powered on** (lists and
  previews are always visible, from the cache).
* You cannot send a message from the home screen — it is read plus jump-to-VS-Code.
* Connector auth is a single shared token. Per-machine tokens and revocation are a later
  step.
* The hub does **not** proxy the individual machines yet. That is why "Open in VS Code"
  requires the machine to have an externally reachable address (`DPX_PUBLIC_URL`). Making
  the hub relay HTTP over the WS tunnel would remove this constraint — the most valuable
  next piece of work.
