"""The DarkPyonix package.

One folder, one concern.

| Folder / file    | Role                                                       |
|------------------|------------------------------------------------------------|
| `config.py`      | Env vars, paths, constants — every setting lives here       |
| `assets.py`      | Helpers that read `static/` files and turn them into responses |
| `auth/`          | Login, sessions, the gate                                   |
| `vscode/`        | Everything VS Code Web — relaying, injection, the companion-extension gate |
| `home/`          | The back end of the home screen — conversation API, recent folders |
| `agents/`        | Agent conversation adapters (Claude Code, Codex)            |
| `hub/`           | The hub showing several machines on one home screen (optional) |

App assembly and request routing live in `main.py` at the repository root.
"""
