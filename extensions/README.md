# Editor extensions

Editor extensions for DarkPyonix notebooks live here [user, 2026-10-03]. Tracking issue:
[#15](https://github.com/DarkPyonix/darkpyonix-ember/issues/15). They talk to the DarkPyonix kernel
manager through its HTTP API (darkpyonix-core `docs/api/manager.openapi.yaml`) and read the notebook
format of darkpyonix-core `docs/FORMAT.md`.

| Extension | Folder | ID | Status |
|---|---|---|---|
| DarkPyonix Notebooks for VS Code | [`vscode-darkpyonix/`](vscode-darkpyonix/README.md) | `darkpyonix.vscode-darkpyonix` | **Working** against a fake manager (unit, module-level integration and in-VS Code e2e tests); not yet run against the real manager. Installed by default in Ember's VS Code runtime. |
| DarkPyonix theme for VS Code | `vscode-darkpyonix-theme/` | — | In progress (separate branch). The DarkPyonix (phoenix) theme, default in Ember's VS Code runtime. |
| DarkPyonix for IntelliJ / PyCharm | `intellij-darkpyonix/` | — | In progress (separate branch). The same notebook features for JetBrains IDEs. |
