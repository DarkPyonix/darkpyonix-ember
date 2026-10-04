# ember-app: Ember's native client (M3, SPEC §L)

The launcher and conversation UI, written in Rust on
[dioxus-compose](https://github.com/DarkPyonix/dioxus-compose): `rsx!` components drawn by a
Compose renderer, with **no webview** (E1, NFR-L2). All data comes from `ember-client`
(`../client`): sync engine, state reducer, transcripts, cache.

## Run

```sh
cd crates/app
EMBER_SERVER_URL=http://127.0.0.1:8740 cargo run      # the main server (default shown)
```

| Variable | Default | Meaning |
|---|---|---|
| `EMBER_SERVER_URL` | `http://127.0.0.1:8740` | Main server base URL |
| `EMBER_DATA_DIR` | `<OS data dir>/ember` | Cache (`client-cache.json`), prefs (`prefs.json`), `exports/` |
| `EMBER_LOG` | `warn,ember_app=info,ember_client=info` | `tracing` filter |
| `DIOXUS_COMPOSE_RENDERER_DIR` | (download) | Use a renderer you built instead of the released one (absolute path) |

The OS data dir is `~/Library/Application Support` on macOS, `$XDG_DATA_HOME` (or
`~/.local/share`) on Linux and `%APPDATA%` on Windows.

### Local dioxus-compose

`Cargo.toml` depends on dioxus-compose by git (`branch = "develop"`) so it does not depend on
where this checkout sits. To build against a local checkout instead:

```sh
cargo run --config 'patch."https://github.com/DarkPyonix/dioxus-compose".dioxus-compose.path="/abs/path/to/dioxus-compose/dioxus-compose"'
```

## Tests

`cargo test` runs the view-model, prefs, config, IDE, export and server-shape unit tests, and
`tests/no_webview.rs` (NFR-L2): no webview crate in the resolved dependency graph on any
target, no webview API named in the app's or `ember-client`'s sources.
`../../scripts/check-no-webview.sh` does the same without compiling (CI job `no-webview`).

## Structure

| File | What |
|---|---|
| `src/main.rs` | One line: `ember_app::launch()` |
| `src/lib.rs` | `launch()`: runtime, `Client::new` (loads cache) + `start`, services, window |
| `src/config.rs` | `EMBER_SERVER_URL`, data paths |
| `src/services.rs` | Global services; `run()` = tokio work then a UI-thread continuation |
| `src/bridge.rs` | `Client::subscribe` / `watch_revision` → sync signals; refresh of accounts, agents, computers |
| `src/server.rs` | Endpoints `ember-client` does not wrap yet: accounts, computers, session computer, IDE targets, create-with-account |
| `src/model.rs` | Pure view models: project badges, rows, search, time, outbox (FR-L6) |
| `src/prefs.rs` | Per-device choices: last-used per project (FR-L8), last project, IDE target (FR-L7) |
| `src/ide.rs` | "Open IDE": URL safety check and OS URL handler |
| `src/export.rs` | Session export (FR-L9): the server's export file plus the reduced transcript |
| `src/ui/mod.rs` | Root: `Scaffold` + adaptive `Navigation` (bar / rail / sidebar), routes |
| `src/ui/launcher.rs` | Main screen (FR-L1–L4) |
| `src/ui/conversation.rs` | Conversation view (FR-L5–L7) |
| `src/ui/new_session.rs` | New session dialog (FR-L8) |
| `src/ui/search.rs` | Search (FR-L9): server full-text hits (FR-S4), then title/detail matches |
| `src/ui/compat.rs` | Stand-ins for widgets dioxus-compose does not have yet |

### Reactivity

Screens read the client's `State` directly (`Client::read`) while rendering and subscribe to
sync **revision signals** (`launcher`, `transcripts`, `outbox`) that the bridge bumps from
tokio threads, the way dioxus-compose's chat sample streams a reply. `Client::subscribe`
gives targeted invalidation (a streamed delta redraws the transcript, not the project grid);
`Client::watch_revision` recovers after the broadcast receiver lags. User actions run on
the runtime and continue on the UI thread through `dioxus_core::spawn` (the notepad sample's
pattern).

## Stand-ins (swap when the widget lands)

| Stand-in (`src/ui/compat.rs`) | Replaced by |
|---|---|
| `Badge`, `Dot`, bullet in nav titles | Chip/badge widget (dioxus-compose M9, ~10-08) |
| `MessageText` (plain `Text`) | `dioxus-compose-markdown` `Markdown` / `MarkdownStream` (~10-14), text selection (M9) |
| `CodeText` (monospace `Text`, wraps) | Syntax highlighting (~10-14), horizontal scroll + selection (M9), code editor widget (FR-38) |
| `SplitPane` (`Row` with weights) | Split pane widget (~10-14) |
| Computers as a vertical list | Horizontal scroll strip (M9) |
| Title `Button` inside `Card` | A clickable `Card` |
| Remembered text shown as placeholder | `TextField` initial value |

## Not done yet, and why

- **Search hit → message**: a hit opens its session; scrolling to the matching message
  (`seq`) waits for a `LazyColumn` scroll-to API.
- **FR-S5 fork**: no agent supports it yet (`can_fork: false`), so the UI offers no action.
- Pins, archive marks and renames kept in an older `prefs.json` are not uploaded to the
  server; they are dropped on the next prefs save.
- **Ember IDE target**: placeholder until the editor core (M8).
- **Auto-scroll** to the newest message: `LazyColumn` has no scroll-to API yet.
