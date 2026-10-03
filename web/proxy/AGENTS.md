# Home screen: folders → agent conversations → transcript

```
   folder card          conversation list          transcript
   ┌──────────┐        ┌──────────────┐          ┌────────────┐
   │ 📁 app    │  tap → │ 💬 auth gate  │  tap →  │  bubbles…   │
   │ 3 convos  │        │ 💬 proxy fix  │          │ [Resume]    │
   │ [VS Code] │        └──────────────┘          └────────────┘
   └──────────┘
```

* The folder list = **folders that have agent conversations** ∪ **recent workspaces the
  server remembers** ∪ **browser bookmarks** (folders added by hand). The three are merged
  by path.
* Conversation lists, previews and full transcripts are built by the server reading
  `~/.claude` and friends directly. VS Code does not have to be running, and this works
  even when `code serve-web` is down.
* The card's **[Open in VS Code]** is the same workspace entry point as before.

## Files

| File | Role |
|---|---|
| `static/home.html` | The whole home screen (folder/conversation/transcript panes + bookmarks + theme) |
| `dpx/agents/base.py` | Adapter interface + shared models (Workspace / Conversation / Message) |
| `dpx/agents/claude_code.py` | Claude Code session parser |
| `dpx/agents/codex.py` | Codex CLI parser (experimental) |
| `dpx/agents/__init__.py` | Registry — merges the adapters into one list |
| `dpx/home/api.py` | The `/__agents/*` API |
| `main.py` | Home serving (middleware) + route assembly |

The screen and the API now live outside `main.py`. Home serves `static/home.html` as-is, and
`/__agents/*` lives in the router in `dpx/home/api.py`. Proxying, injection and auth are
`dpx/vscode/proxy.py`, `dpx/vscode/inject.py` and `dpx/auth/gate.py` respectively — the full
map is in the "Files" section of [README.md](README.md).

## API (all of it requires a login)

| Endpoint | Description |
|---|---|
| `GET /__agents/workspaces` | Folder list + the 3 most recent conversations per folder + available agents |
| `GET /__agents/conversations?folder=` | One folder's conversation list (title, time, preview) |
| `GET /__agents/transcript?agent=&cid=` | One conversation's full transcript |
| `GET /__agents/resume?agent=&cid=` | The resume command (`claude --resume <id>`, etc.) |

## What changed about auth

Because home shows folder paths and conversation previews, **home (`/`) and `/__workspaces`
now require a login too** — they used to be public, which meant the machine's folder paths
were visible before signing in. The `POST /__login` stub, which let any username through,
was deleted. The real login is `POST /auth/login`, as before.

## What "Resume" actually does

There is no way for the browser to type a command into a terminal inside VS Code on the
user's behalf. So:

1. Home fetches the command (`claude --resume <id>`) from `/__agents/resume` and **copies it
   to the clipboard**
2. Navigates to `/?folder=…&agent=…&cid=…`
3. The frame wrapper opens the workspace and **opens the agent panel automatically**
4. Pasting into the terminal resumes that conversation

Doing it in one button press would require the server to run the terminal on the user's
behalf, and that becomes a channel for running arbitrary commands remotely — so a folder
allowlist has to come first.

## Adding a new agent

Implement `AgentAdapter` from `dpx/agents/base.py` and add one line to `_CANDIDATES` in
`dpx/agents/__init__.py`. That is all — the home screen and the API need no changes.

```python
class MyAgentAdapter(AgentAdapter):
    id = "my-agent"; name = "My Agent"
    def available(self): ...                    # does this machine have any history?
    def conversations(self, folder=None): ...   # the list (title, time, preview)
    def transcript(self, cid, limit=300): ...   # the full transcript
    def resume(self, cid): ...                  # {"kind":"terminal","command":…,"cwd":…}
```

`workspaces()` already has a default implementation that groups `conversations()` by folder.

| Adapter | Reads from | Status |
|---|---|---|
| `claude-code` | `~/.claude/projects/*/*.jsonl` | Supported — titles, full transcripts, `claude --resume` |
| `codex` | `~/.codex/sessions/**/*.jsonl` | Experimental — silently skips anything in an unexpected format |

To make it look for session files elsewhere, point the `DPX_HOME` environment variable at a
different home directory.

## Notes

* When `code serve-web` is down, opening a workspace shows **an explainer page** instead of a
  blank 500. Home and the transcripts keep working in that state.
* To see several machines on one home screen, see `docs/HUB.md` (optional).
