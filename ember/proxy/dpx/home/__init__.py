"""The back end of the home screen (static/home.html).

| File            | Role                                             |
|-----------------|--------------------------------------------------|
| `api.py`        | The `/__agents/*` and `/__workspaces` JSON API    |
| `workspaces.py` | The record of recently opened workspaces (`_xmo_recent.json`) |

Reading the conversations themselves is the job of the `dpx/agents/` adapters.
"""
