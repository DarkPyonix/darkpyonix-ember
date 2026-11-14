# DarkPyonix Notebooks for VS Code

Extension ID: **`darkpyonix.vscode-darkpyonix`** (publisher `darkpyonix`, name `vscode-darkpyonix`).

Opens DarkPyonix notebook files (`.pynb`, and `.py` on request) as VS Code notebooks, runs them on
the file's DarkPyonix kernel, and lets several clients (VS Code, IntelliJ, ash, Ember, agents)
edit the same notebook together.

Contract (darkpyonix-core, read-only here): `docs/FORMAT.md`, `docs/api/manager.openapi.yaml`
(1.0.0-draft.4), `docs/SPEC.md` §7–§10a, `docs/PROTOCOL.md` §3.4 and §4.

## What it does

| Feature | Requirement | Where |
|---|---|---|
| `.pynb` opens as a notebook; any `.py` via **Open as DarkPyonix Notebook** (explorer / editor title context menu, command palette) | FORMAT §2 | `src/vscode/serializer.ts` |
| Byte-exact save: untouched cells, CRLF, BOM, missing final newline, headers with trailing spaces all survive | FR-F1 | `src/format/` |
| `[markdown]` cells show the `darkpyonix.markdown("""…""")` body as a Markdown cell; the call (and `silent=`) is kept on save | FORMAT §3.2 | `src/format/cells.ts` |
| Other types (`argparse`, `binding`, `shell`, `parallel`, `concurrent`, `*interop`, unknown) are code cells with a type badge; the preamble is shown as the first cell when it is not empty | FORMAT §3 | `src/vscode/status.ts` |
| Finds a manager like the CLI: `darkpyonix.manager.url` + token, else `DARKPYONIX_MANAGER_URL` + `DARKPYONIX_TOKEN` (only one of the pair is an error), else spawns `darkpyonix manager --ephemeral` and reads `{url, token, pid}` from its first stdout line. No registry file; the spawned manager's stderr goes to `<home>/manager.log`, it is left running when VS Code quits and ends after its idle timeout, and a new one is spawned if it has ended | FR-C1, FR-M3, FR-M4 | `src/manager/discovery.ts` |
| Controller **DarkPyonix**: start/attach (`POST /kernels`), run cells by `cell_ids`, stop = interrupt, restart (soft/hard), graceful shutdown. Never `force=true` | FR-M2, FR-X1, FR-X4, FR-K7, FR-K8 | `src/connection.ts`, `src/extension.ts` |
| Live outputs from SSE `output` events (stream, display_data, execute_result, error), `output.clear` | PROTOCOL §3.4 | `src/run/` |
| Latest outputs on open, with an "outputs from previous source" badge on stale cells | FR-R4 | `src/connection.ts` |
| "run by user@device" / "queued by …" on running cells, for anyone's runs | FR-S6 | `src/run/tracker.ts` |
| Collaboration: snapshot + SSE from `seq` with `client_id`/`nickname`; remote `doc.cell.*` / `doc.reloaded` applied without echo; lock on typing, renew, release on leaving the cell or after idle (with the final source); edits with `base_version`; 409 `locked` undoes the change and names the holder; 409 `conflict` offers *Keep mine* / *Use theirs*; `doc.conflict` offers *Keep editor version* / *Use disk version*; others' locks, focus and cursors as badges and inline labels; own focus/cursor via throttled `PUT presence` | FR-S1..S5, FR-S8 | `src/collab/` |

## Settings

| Setting | Default | |
|---|---|---|
| `darkpyonix.cliPath` | `darkpyonix` | CLI used to spawn an ephemeral manager |
| `darkpyonix.managerArgs` | `["manager", "--ephemeral"]` | its arguments |
| `darkpyonix.home` | (empty) | runtime home (where `manager.log` goes, passed to a spawned manager); empty = `$DARKPYONIX_HOME` or `~/.darkpyonix` |
| `darkpyonix.manager.url` / `darkpyonix.manager.token` | (empty) | dedicated manager; **Connect** stores the token in the OS secret store instead |
| `darkpyonix.manager.pathMap` | `{}` | local path prefix → path on the dedicated manager's machine |
| `darkpyonix.autoStartKernel` | `false` | start the kernel (and join collaboration) on open; a kernel that is already running is always attached |
| `darkpyonix.python` | (empty) | interpreter for new kernels |
| `darkpyonix.nickname` | host name | device name shown to others |
| `darkpyonix.lockIdleSeconds` | `20` | release an edit lock after this long without typing |

The client id is generated once per machine (VS Code global state) and sent as
`X-DarkPyonix-Client` / `?client_id=`.

## Design notes

- **Echo-free sync.** The session keeps the server's version of every cell. Remote changes go into
  that model first and then into the editor, so the editor-change callback that follows finds no
  difference and sends nothing. Only editor-vs-model differences are sent.
- **Cell ids live outside the file.** The kernel `cell_id` of each editor cell is kept by cell
  identity (a `WeakMap`), not in cell metadata, because a metadata edit dirties the notebook and a
  copied cell would carry the id. When VS Code reloads a clean notebook from disk (the kernel
  rewrites the file after every edit, FR-S5) its cells are replaced by new ones; unbound cells that
  match a model cell are re-adopted, and creates/deletes are only sent after they have been stable
  for 1 s, so a reload never turns into delete + create.
- **The kernel writes the file** while collaboration is on (FR-S5). The notebook still shows as
  dirty after edits; saving writes the same bytes. If VS Code reports that the file on disk is newer,
  *Overwrite* is safe.
- Editor-agnostic modules (`format/`, `manager/`, `collab/`, `run/`, `connection.ts`) have no
  `vscode` import, so they are tested directly and can be reused by other clients.

## Develop

```sh
npm install
npm run typecheck
npm test            # vitest: format round trip, event application, client + session + runs against a fake manager
npm run build       # esbuild → dist/extension.js
npm run test:e2e    # the real extension inside an installed VS Code (VSCODE_PATH; nothing is downloaded)
DARKPYONIX_CLI=/path/to/darkpyonix npm run test:real   # client, runs and outputs against the real manager
npm run package     # → vscode-darkpyonix-<version>.vsix
```

`npm run test:real` (`test/real/`) spawns the real `darkpyonix manager --ephemeral` named by
`DARKPYONIX_CLI` with a temporary `DARKPYONIX_HOME` under `.scratch/`, and checks: discovery from
the stdout line, opening a `.pynb` (kernel start, cells bound), running a cell, live SSE output while
a cell still runs, interrupting a slow cell (the kernel process and namespace survive), the latest
outputs per cell on reopen, and the stale mark after an edit. It ends the kernel and the manager it
started. CI (`test-vscode-extension.yml`, job "Notebook extension against the real manager") builds
the manager from a pinned darkpyonix-core commit (`CORE_REF`) and runs it.

## Manual check (owner)

Checks that need a person at the screen; CI does not cover them. Install the built `.vsix`
(`npm run package`, then *Extensions: Install from VSIX...*) and have the `darkpyonix` CLI on `PATH`
(or set `darkpyonix.cliPath`).

1. **Manager found or spawned like the CLI.** With no `darkpyonix.manager.url` and no
   `DARKPYONIX_MANAGER_URL`/`DARKPYONIX_TOKEN`, open a notebook: *DarkPyonix: Show Log* says
   `manager: spawned http://127.0.0.1:... (pid N)` and `<home>/managers/` does not appear. Start
   VS Code from a shell where `darkpyonix manager` printed `{url, token, pid}` and both variables are
   exported: the log says `manager: env ...` and no new manager is spawned. Export only one of the
   two: opening fails with "set both DARKPYONIX_MANAGER_URL and DARKPYONIX_TOKEN, or neither".
2. **Open.** A `.pynb` file opens as a notebook; a `.py` file opens as one through *Open as
   DarkPyonix Notebook* (explorer context menu). Cells, titles, Markdown cells and type badges match
   the file.
3. **Latest outputs per cell.** For a file that has run before (`darkpyonix run file.pynb`), every
   cell shows the outputs of the latest run on open. Edit a cell: it shows the "outputs from previous
   source" badge; other cells do not.
4. **Run.** Run one cell, then *Run All*: outputs appear in the cells, and the file's `__runs__/`
   gets a new run log.
5. **Live outputs.** Run `for i in range(5): print(i, flush=True); time.sleep(1)`: the numbers
   appear one per second, not at the end.
6. **Stop.** Run `time.sleep(600)` and press the stop button (*Interrupt Kernel*): the cell ends
   with `KeyboardInterrupt`, and running `print(x)` for a variable set earlier still works (same
   kernel, namespace kept).
7. **Leave.** Quit VS Code: the kernel keeps running (`darkpyonix ps`), and the spawned manager
   ends by itself after its idle timeout (2 minutes).

`test/corpus/darkpyonix_format.py` is a copy of darkpyonix-core `docs/examples/darkpyonix_format.py`
(FR-F1 reference file); keep it in sync when the format changes.

## License

Apache-2.0, like the rest of this repository (`LICENSE`).
