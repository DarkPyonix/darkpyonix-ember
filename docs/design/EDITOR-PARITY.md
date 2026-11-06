# Ember editor parity with Code-OSS (OSE)

The owner's rule [user, 2026-11-06, `INTENT.md` D38]: the Ember editor does what Code-OSS does and looks like the mockup (`.scratch/mockup-v8/editor.html`). This file is the row-by-row account of the functions. It is written from the code on `develop` at the time of writing (read, not compiled, not run on a screen); a row changes when its issue lands, in the same commit.

**Columns.** *OSE behaviour*: what Code-OSS does (general knowledge; the OSE build here changes only `product.json` (see `ember/ose/README.md`), so every function exists in the server and workbench in principle). *Ember today*: 있음 (works in code), 부분 (part), 없음 (not there). *Connection*: how a native Ember window reaches it through `ember-editor-conn` (`docs/design/EDITOR-CONNECTION.md`): **EXISTS** already exposed; **FS-CH** the existing `remoteFilesystem` channel suffices; **EH-CALL** typed ExtHost* call builders plus engine and widget code, ids already pinned, no new wire protocol; **EH-REG** decode of a MainThread* registration the connection layer today answers with a default reply; **WEBVIEW** needs a hosted web region (the IDE window may hold one, `INTENT.md` E5); native work in Ember or in the compose-rust widget needs no connection. *Issue*: the open issue that closes the row.

**What the connection layer exposes today** (`ember/editor-conn/src`): the management channels `remoteFilesystem`, `remoteextensionsenvironment` and `remoteExtensionsScanner` (the generic IPC client can call any channel by name, none other is wrapped); the extension-host connection with all 168 proxy ids pinned, but MainThread calls are decoded only for diagnostics, hover, CodeLens, inline completions and command registration; every other call gets a default reply (workspace always trusted, `$checkExists` false, message and quick pick auto-dismissed). `EditorSession` offers read_dir, stat, open, close, focus, widget events, inline completion and resync. The terminal does not use the OSE pty host; it uses ember node (`terminal_stream.rs`).

**What the window does today** (`ember/app/src/ui/editor_workbench.rs`): a separate process `--editor <project> <folder>`; the CodeEditor gets only text, decorations and tab width; the top bar, stripes, tabs, one terminal and the status bar exist; the stripes Commit, Git, Agent and Notifications and the actions Find, Palette, Recent, Format, Run post 준비 중. Nothing tests the rendered window; live tests against OSE are `#[ignore]` and run by hand.

| Function | OSE behaviour | Ember today | Connection (ember-editor-conn) | Issue |
|---|---|---|---|---|
| Open folder, explorer tree | Explorer view over the file service; expand, create, rename, delete, copy, drag | 부분 | FS-CH: the `remoteFilesystem` channel (stat, readdir, mkdir, delete, rename, writeFile, watch) is wrapped in `RemoteFs`; `EditorSession` exposes only read_dir and stat | #438 |
| Tabs (open, close, dirty) | Editor groups: pin, preview, split, reorder, close others, confirm on dirty close | 부분 (open, close, dirty dot 있음; pin, split, reorder 없음) | In the window (no wire) | #439, #305 (label clipped) |
| Editing core: undo, clipboard, auto-indent | Monaco | 있음 (CodeEditor widget) | Widget in the renderer; edits go `on_change` -> bridge -> extension-host mirror | - |
| Editing core: find/replace, multi-cursor, minimap, word wrap, bracket match | Monaco | 없음 | Renderer widget work (compose-rust), then wiring | #440 |
| Syntax highlighting, semantic tokens, folding | TextMate grammars and semantic tokens; fold providers | 없음 (folding 부분: the widget folds, Ember supplies none) | Grammars from the scanned extensions or ExtHostLanguageFeatures; Ember passes `syntax_spans` and `folds` | #441 |
| Themes, light and dark | Theme service applies colour themes and icon themes | 없음 (hard-coded dark colours) | Native tokens of the mockup (light and dark); the OSE theme data only where useful | #300 (absorbed by the window rewrite) |
| Diagnostics (squiggles) | MainThreadDiagnostics | 있음 (underlines) | EXISTS: `$changeMany`/`$clear` in editor-conn | - |
| Hover | Hover providers | 부분 (data requested and merged, no popup) | EXISTS: `$provideHover` | #442 |
| CodeLens, inline completion (ghost text) | Language providers | 있음 in the session, on-screen unverified | EXISTS | - |
| Completion list, definition, references, rename, code actions, format, signature help | Language providers and LSP clients in the extension host | 없음 | EH-REG decoding of `$register*` plus ExtHost* call builders (ids are pinned) | #442 |
| Search across files, replace | Ripgrep through the extension host search provider | 없음 | MainThreadSearch/ExtHostSearch (ids only) | #443 |
| SCM / git view, diff gutter | Built-in git extension, MainThreadSCM | 없음 (stripes post 준비 중) | EH-REG for MainThreadSCM, or Ember's own git API (FR-L19) | #444 |
| Debug | DAP through the extension host, launch.json | 없음 | MainThreadDebugService (id only); scope decision | #445 |
| Extensions panel, install | Extension management service, Open VSX | 없음 (extensions run, no panel) | Management channels `remoteextensionsenvironment` and `remoteExtensionsScanner` are wrapped; install through the server CLI via ember node | #446 |
| Integrated terminal | Server pty host, profiles, tasks, shell integration | 부분 (one terminal over ember node as plain text lines) | ember node terminals (not the OSE pty host), `terminal_stream.rs` | #334 (resize), #335 (paste, IME), #336 (reconnect), #337 (list, kill), #333 (TLS), #26 |
| Command palette, quick open | Quick-access over all commands, files and symbols | 없음 (menu items post 준비 중) | Commands from `$registerCommand` (EXISTS) plus Ember's; files from the explorer or search | #447 |
| Settings UI, settings.json | Settings editor, profiles | 없음 (only the keymap choice) | Native screen; `SessionConfig.user_settings` feeds the extension host once | #449 |
| Keybindings, keymap | Keybinding service and editor | 부분 (VS Code and JetBrains tables, 19 actions, most post 준비 중) | Native (FR-L30) | #447, #449 |
| Problems panel, Output panel | Marker service list; output channels | 없음 (static text) | EH-REG for output channels; diagnostics data EXISTS | #448 |
| Notifications, progress, extension quick picks | MainThreadMessageService, Progress, QuickOpen | 없음 (auto-dismissed with default replies) | EH-REG | #448 |
| Status bar, language mode, encoding, EOL | Status bar items | 부분 (cursor position live, LF and UTF-8 constants) | Native plus document data | #453 |
| Breadcrumbs | Path and symbols | 부분 (path only) | Document symbols need the language providers | #453, #442 |
| Workspace trust | Trust service and restricted mode | 없음 (always trusted) | EH-REG `$requestWorkspaceTrust` plus a native dialog | #450 |
| Save, auto-save, hot exit | Backup service, auto-save setting | 부분 (save 있음; failure not shown; no auto-save or hot exit) | Native; write through `remoteFilesystem` | #451 |
| File watching, external change | Server file watcher | 부분 (session reloads clean buffers; the window shows nothing) | EXISTS: watch and fileChange | #451 |
| Webviews, custom editors, notebooks, markdown preview | Origin-isolated iframes from the webview host | 없음 | WEBVIEW only (E5 allows one in the IDE window); decision first | #452 |
| Remote development (SSH, WSL, containers) | Closed Microsoft remote extensions | 부분 (different model: the OSE server on the node is the remote agent) | Handshake and version pin (`PINNED_COMMIT`), fallback `Open in VS Code` | - |
| Accessibility | ARIA in the DOM, a11y help, audio cues | 없음 | Native roles and focus order | #302 |
| Layout, window resize | - | 부분 | dioxus-compose page | #301, #320 |
| Localisation | Language packs as extensions | 부분 (the extension host gets the language; no packs) | scanExtensions language pack | - |

## Notes

- The fallback `Open in VS Code` (`ember/editor/src/fallback.rs`, the proxy-wrapped web workbench in `ember/proxy`) is the only path today where every row works, at the price of a webview; it stays the fallback, not the design (`INTENT.md` D9a).
- Version risk: proxy ids and signatures change per Code-OSS release; each typed call is checked at the pinned commit (CI job `editor-conn-pin`).
- Unknown (not inspected): the contents of a built OSE archive; whether settings sync, remote tunnel or the webview host work in a built OSE.
- The umbrella issue is #32. Rows are not closed by this file: each issue states its own done condition.
