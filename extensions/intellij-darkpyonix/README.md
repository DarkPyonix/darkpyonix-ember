# intellij-darkpyonix

DarkPyonix notebooks in PyCharm and IntelliJ IDEA: the IntelliJ counterpart of the VS Code
extension, speaking the same manager protocol.

- Plugin id: `dev.darkpyonix.intellij` ("DarkPyonix Notebooks")
- Platform: IntelliJ Platform Gradle Plugin 2.19.0, Kotlin 2.4.20, Gradle 9.8.0, JDK 21.
  Compiled against IntelliJ IDEA 2025.3; `sinceBuild = 253`, no `untilBuild`. 2025.3 is the
  last release with separate PyCharm Community / IntelliJ IDEA Community distributions and
  the first unified one, so one build covers PyCharm Community 2025.3 and every unified
  PyCharm / IntelliJ IDEA since (current stable: 2026.2.3, build 262).
- Contract: darkpyonix-core `docs/FORMAT.md`, `docs/api/manager.openapi.yaml`
  (1.0.0-draft.3), `docs/SPEC.md` §7–§10a, `docs/PROTOCOL.md` §3.4 and §4.

## Build, test, run

```sh
cd extensions/intellij-darkpyonix
./gradlew buildPlugin      # build/distributions/intellij-darkpyonix-0.1.0.zip
./gradlew test             # JUnit 4 unit tests (parser, events, sync, SSE, JSON)
./gradlew runIde           # sandbox IntelliJ IDEA 2025.3 with the plugin
./gradlew runPyCharm       # sandbox PyCharm Community 2025.3 with the plugin
./gradlew verifyPlugin     # optional; needs pluginVerification { ides { ... } } first
```

Install the zip with *Settings | Plugins | ⚙ | Install Plugin from Disk…*.

## What it does

| Feature | Requirement | Where |
|---|---|---|
| Cells from `# %% [type]` markers, `# @key: value` metadata, preamble = cell 0, `source_sha256` | FORMAT §2 | `format/NotebookFormat.kt` (port of the kernel's `format/_parser.py`) |
| Which files: `.pynb` always, `.py` with at least one marker; `.pynb` is a Python file when Python support is installed | FORMAT §1 | `notebook/NotebookService.kt`, `META-INF/darkpyonix-python.xml` |
| Run / interrupt per cell from the gutter (icon turns into stop while the cell runs); Run Cell (`Ctrl+Alt+Shift+Enter`), Run All, Interrupt, Start/Attach, Restart in the editor menu and Tools | FR-X*, FR-S6 (`cell_ids`) | `editor/CellDecorator.kt`, `actions/` |
| Busy kernel: queue (default) or refuse with "Queue / Interrupt" choices | FR-X3 (`on_busy`) | `NotebookSession.runNow` |
| Latest outputs per cell, mapped by the manager's document snapshot, kept live from SSE (`cell.started`, `output`, `output.clear` with `wait`, `cell.finished`, `run.*`); stale outputs are marked | FR-R4, PROTOCOL §3.4 | `protocol/DocumentState.kt`, `toolwindow/OutputsToolWindow.kt` |
| Manager discovery: `$DARKPYONIX_HOME/managers/*.json` (dedicated first, newest first, pid alive, `/health`), else `darkpyonix manager --ephemeral` and wait for its registration | FR-C1, FR-M3 | `manager/ManagerDiscovery.kt` |
| Kernel: attach on open only if it already runs (`GET /kernels/k_<sha>`); start with `POST /kernels` on first run | FR-M2, PROTOCOL §2.6 | `NotebookSession.connect` |
| Event stream with `client_id`/`nickname`, resume with `Last-Event-ID`, resync on `replay_truncated` | FR-M1, FR-S4, PR-3 | `manager/EventStream.kt` |
| Shared document: snapshot + events converge (create/update/delete/move, `doc.reloaded`, `doc.conflict`) | FR-S1, FR-S5 | `DocumentState.apply` |
| Edits: lock on first keystroke in a cell, push 300 ms after typing stops with `base_version`; inserted / deleted / reordered cells become create / delete / move | FR-S2, FR-S3 | `protocol/CellSync.kt`, `NotebookSession.flush` |
| Locks: release when the caret leaves the cell or after 60 s idle; cells locked by others are read-only (guarded) and tinted, with "locked by …" | FR-S3 | `NotebookSession`, `CellDecorator` |
| Conflicts: `409 conflict` → "Keep mine" (re-apply on the new version) / "Take theirs"; `409 locked` → "Take theirs" | FR-S2 | `NotebookSession.onEditRejected` |
| Presence: focus and cursor (`PUT /presence`, ≤ 10/s), others' focus and line shown at the cell marker and in the tool window | FR-S4 | `NotebookSession.onCaret`, `CellDecorator` |
| Permissions from `GET /api/manager`: edits only for `editor`/`admin`; `viewer3` runs the buffer via `source` | FR-A3, FR-S8 | `NotebookSession` |

Settings: *Settings | Tools | DarkPyonix* (CLI path, spawn arguments, dedicated manager URL
and token, Python for new kernels, nickname, attach-on-open, queue-when-busy). The client id
is generated once per installation.

## Layout

```
src/main/kotlin/dev/darkpyonix/intellij/
  format/      NotebookFormat     parser, sha256, kernel id, header formatting (pure)
  json/        Json               tiny JSON codec (pure; no IDE dependency)
  protocol/    DocumentState      snapshot + event application (pure)
               CellSync           FR-R4 matching and edit reconciliation (pure)
               SseParser, Models
  manager/     ManagerClient      HTTP client for manager.openapi.yaml
               ManagerDiscovery   managers/*.json and ephemeral spawn
               EventStream        SSE connection with Last-Event-ID
  notebook/    NotebookService, NotebookSession   IDE glue: sync, locks, presence, runs
  editor/      CellDecorator, NotebookEditorFactoryListener
  toolwindow/  OutputsToolWindow
  actions/     NotebookActions
  settings/    DarkPyonixSettings, DarkPyonixConfigurable
src/test/kotlin/...  JUnit 4 tests for the pure packages
```

## Not verified yet (no Gradle build was run when this was written)

- Compilation as a whole, the Gradle/IntelliJ Platform plugin DSL (`intellijIdea(...)`,
  `intellijPlatformTesting.runIde.register("runPyCharm")` with
  `IntelliJPlatformType.PyCharmCommunity`), and `opentest4j` being needed for tests.
- `fileType name="Python" extensions="pynb"` in the optional descriptor adding an extension
  to the existing Python file type.
- Guarded blocks blocking typing in cells locked by others.
- The snapshot `seq` is taken as "last event included" (subscribe with `since = seq`, matching
  the kernel's `welcome.seq` convention); if the manager means "next event", one event can be
  missed and the client should subscribe with `seq - 1`.
- `doc.cell.deleted` is accepted with `cell_id` at the top level or inside `cell`.
- The file on disk: after pushing edits (and after mirroring others' edits) the plugin saves the
  buffer so the kernel's 300 ms save writes identical bytes; whether the kernel's watcher treats
  our save as an outside edit (and sends `doc.reloaded`) needs an end-to-end check.
- Titles cannot be edited remotely (`CellEdit` has no `title`), so a new cell's title only lives
  in the editor buffer until the kernel re-reads the file.

## License

Apache-2.0, like the rest of this repository (`LICENSE`).
