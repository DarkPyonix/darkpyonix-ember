# Ember editor parity with Code-OSS (OSE)

The owner's rule [user, 2026-11-06, `INTENT.md` D38]: the Ember editor does what Code-OSS does and looks like the mockup (`.scratch/mockup-v8/editor.html`). This file is the row-by-row account of the functions. It is written from the code on `develop` at the time of writing (read, not compiled, not run on a screen); a row changes when its issue lands, in the same commit.

**Columns.** *OSE behaviour*: what Code-OSS does (general knowledge; the OSE build here changes only `product.json` (see `ember/ose/README.md`), so every function exists in the server and workbench in principle). *Ember today*: 있음 (works in code), 부분 (part), 없음 (not there). *Connection*: how a native Ember window reaches it through `ember-editor-conn` (`docs/design/EDITOR-CONNECTION.md`): **EXISTS** already exposed; **FS-CH** the existing `remoteFilesystem` channel suffices; **EH-CALL** typed ExtHost* call builders plus engine and widget code, ids already pinned, no new wire protocol; **EH-REG** decode of a MainThread* registration the connection layer today answers with a default reply; **WEBVIEW** needs a hosted web region (the IDE window may hold one, `INTENT.md` E5); native work in Ember or in the compose-rust widget needs no connection. *Issue*: the open issue that closes the row.

**What the connection layer exposes today** (`ember/editor-conn/src`): the management channels `remoteFilesystem`, `remoteextensionsenvironment` and `remoteExtensionsScanner` (the generic IPC client can call any channel by name, none other is wrapped); the extension-host connection with all 168 proxy ids pinned, but MainThread calls are decoded only for diagnostics, hover, CodeLens, inline completions and command registration; every other call gets a default reply (workspace always trusted, `$checkExists` false, message and quick pick auto-dismissed). `EditorSession` offers read_dir, stat, open, close, focus, widget events, inline completion and resync. The terminal does not use the OSE pty host; it uses ember node (`terminal_stream.rs`).

**What the window does today** (`ember/app/src/ui/editor_workbench.rs`): a separate process `--editor <project> <folder>`; the CodeEditor gets only text, decorations and tab width; the top bar, stripes, tabs, one terminal and the status bar exist; the stripes Commit, Git, Agent and Notifications and the actions Find, Palette, Recent, Format, Run post 준비 중. Nothing tests the rendered window; live tests against OSE are `#[ignore]` and run by hand.

| Function | OSE behaviour | Ember today | Connection (ember-editor-conn) | Issue |
|---|---|---|---|---|
| Open folder, explorer tree | Explorer view over the file service; expand, create, rename, delete, copy, drag | 부분 (the tree is a flat list; a folder on another computer cannot be opened at all, see Server and node) | FS-CH: the `remoteFilesystem` channel (stat, readdir, mkdir, delete, rename, writeFile, watch) is wrapped in `RemoteFs`; `EditorSession` exposes only read_dir and stat | #438 |
| Tabs (open, close, dirty) | Editor groups: pin, preview, split, reorder, close others, confirm on dirty close | 부분 (open, close, dirty dot 있음; pin, split, reorder 없음) | In the window (no wire) | #439, #305 (label clipped) |
| Editing core: undo, clipboard, auto-indent | Monaco | 있음 (CodeEditor widget) | Widget in the renderer; edits go `on_change` -> bridge -> extension-host mirror | - |
| Editing core: find/replace, multi-cursor, minimap, word wrap, bracket match | Monaco | 부분: 현재 파일 찾기/바꾸기(대소문자, 단어 옵션, 결과 상한 10000 장식 보관, 상한 초과 결과 탐색 가능, 전체 바꾸기는 되돌리기 한 번이 아님: compose-rust#496)는 있고 정규식, 선택 영역 안 찾기, 모두 강조, 다중 커서, 미니맵, 괄호 일치는 없음 | Renderer widget work (compose-rust), then wiring | #440, #628 |
| Syntax highlighting, semantic tokens, folding | TextMate grammars and semantic tokens; fold providers | 없음 (folding 부분: the widget folds, Ember supplies none) | Grammars from the scanned extensions or ExtHostLanguageFeatures; Ember passes `syntax_spans` and `folds` | #441 |
| Themes, light and dark | Theme service applies colour themes and icon themes | 없음 (hard-coded dark colours) | Native tokens of the mockup (light and dark); the OSE theme data only where useful | #300 (absorbed by the window rewrite) |
| Diagnostics (squiggles) | MainThreadDiagnostics | 있음 (underlines) | EXISTS: `$changeMany`/`$clear` in editor-conn | - |
| Hover | Hover providers | 부분 (data requested and merged, no popup) | EXISTS: `$provideHover` | #442 |
| CodeLens, inline completion (ghost text) | Language providers | 있음 in the session, on-screen unverified | EXISTS | - |
| Completion list, definition, references, rename, code actions, format, signature help | Language providers and LSP clients in the extension host | 없음 | EH-REG decoding of `$register*` plus ExtHost* call builders (ids are pinned) | #442 |
| Search across files, replace | Ripgrep through the extension host search provider | 없음 | MainThreadSearch/ExtHostSearch (ids only). The node has `POST /fs/glob` and `/fs/grep` but no server route reaches them from the editor: #456 | #443 |
| SCM / git view, diff gutter | Built-in git extension, MainThreadSCM | 없음 (stripes post 준비 중) | EH-REG for MainThreadSCM, or Ember's own git relay `POST /api/projects/{name}/git/{op}` (FR-L19), which the editor does not call | #444 |
| Debug | DAP through the extension host, launch.json | 없음 (works only in the VS Code Web fallback through the companion extension) | MainThreadDebugService (id only, default reply); the companion extension is browser-only (no `main`), so it does not run in the Ember editor's Node extension host | #445 |
| Extensions panel, install | Extension management service, Open VSX | 없음 (extensions run, no panel) | Management channels `remoteextensionsenvironment` and `remoteExtensionsScanner` are wrapped; install through the server CLI via ember node | #446 |
| Integrated terminal | Server pty host, profiles, tasks, shell integration | 부분 (one terminal over ember node as plain text lines) | ember node terminals through the server relay `/api/computers/{id}/terms` (not the OSE pty host), `terminal_stream.rs` | #334 (resize), #335 (paste, IME), #336 (reconnect), #337 (list, kill), #333 (TLS), #26, #455 (local computer) |
| Command palette, quick open | Quick-access over all commands, files and symbols | 없음 (menu items post 준비 중) | Commands from `$registerCommand` (EXISTS) plus Ember's; files from the explorer or search | #447 |
| Settings UI, settings.json | Settings editor, profiles | 없음 (only the keymap choice) | Native screen; `SessionConfig.user_settings` feeds the extension host once | #449 |
| Keybindings, keymap | Keybinding service and editor | 부분 (VS Code and JetBrains tables, 19 actions, most post 준비 중) | Native (FR-L30) | #447, #449 |
| Problems panel, Output panel | Marker service list; output channels | 있음 (Problems panel with caret navigation, 200 cap and overflow load-more; Output panel with channel selector, logs and clear) | EH-REG for output channels; diagnostics data EXISTS | #448 |
| Notifications, progress, extension quick picks | MainThreadMessageService, Progress, QuickOpen | 부분 (MainThreadMessageService $showMessage decoded and surfaced to notices/logs; Progress, QuickOpen auto-dismissed) | EH-REG | #448 |
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

## Server and node (facts from the server side, `develop` bc0f328)

Written from `.scratch/pz/editor-parity-server-facts.md` (file and line evidence there; nothing built or run). Where the code does not say, the word is **unknown** and stays unknown here.

### Blockers

1. **The OSE runs on the server machine only.** `ember/server/src/ose.rs` spawns one OSE process per canonical project folder, on the server (`--host 127.0.0.1`, an OS-chosen port that is read and released before the child binds it, a 64-hex connection token). `ember/server/src/api/ide.rs:263-266` refuses a non-local computer with "OSE server is only available on the local computer"; the node has no OSE or `/ide` route (`ember/node/src/api.rs:147-203`). So remote files, language servers, file watching and extensions are all relative to the server's disk. How the OSE behaves when several folders share the default data dir (no `--server-data-dir`, `ose.rs:65`) is unknown. Since #459 every OSE gets `--server-data-dir <ose data root>/<computer id>` (see "Task 6, built" below); whether several OSE processes may share that one dir is still unknown.
2. **The endpoint is a plain-TCP loopback address that the app dials itself** (`http://127.0.0.1:<port>`, `ember/editor/src/session.rs:112-120`). A device on another machine connects to itself. No relay, TLS or transport carries the OSE socket.
3. **The IDE computer list is hand-written** (`EMBER_IDE_COMPUTERS`, TODO at `ide.rs:18-21`), separate from the computers registry, while the terminal relay expects registry ids; whether the names match is unknown. The terminal relay answers 400 for the local computer. **Resolved by #454:** the IDE computers come from the registry and the session's current computer, and `launch.computer` is the registry id; `EMBER_IDE_COMPUTERS` is only an override. For the local computer the id is `local`, so the terminal relay still answers 400 until #455.
4. **Search and SCM have node routes but no editor path:** `POST /fs/glob`, `/fs/grep` on the node, the server's git relay; the editor calls none of them.
5. **The companion extension is browser-only** (`package.json` declares `browser`, no `main`), so tasks, debug and the `ember-term` automation profile run only in the VS Code Web fallback, not in the Ember editor's Node extension host; `ember-editor-conn` answers `MainThreadTask*` and `MainThreadDebugService` with default replies.
6. **Client limits:** one workspace folder (`session.rs:205`), no reconnect (the building block `reconnect_loop` exists), no configuration change at runtime.

### Unblock order (server and node)

| # | Task | Size | Unblocks | Issue |
|---|---|---|---|---|
| 1 | IDE computer from the registry and the session's current computer | small | terminal for any computer; prerequisite for 2 and 4 | #454 (built in the PR: `ide.rs` resolves registry plus `EMBER_IDE_COMPUTERS`; `launch.computer` is the registry id; tests `fr_w4_ide_computers.rs`) |
| 2 | In-process node for the local computer's terminal relay | small to medium | terminal and tasks on the server machine | #455 (built in the PR, not yet compiled or run: `computers/local_node.rs`, tests `fr_w4_local_terms_*`. Security note: it adds a loopback HTTP listener on the server machine, started on first terminal use, with a random in-memory token (256 bits) that equals the server token in power; another local OS user can connect but not authenticate; it is reachable only through the relay routes behind `auth::protect`) |
| 3 | Server relay of node file operations incl. glob and grep | small to medium | native search, tree and read/write for other computers | #456 (built for the read-only subset: `stat`, `lstat`, `read`, `list`, `readlink`, `glob`, `grep` at `POST /api/computers/{id}/fs/{op}`, `computers/fs.rs`, tests `fr_w4_fs_relay_*`, not yet run; the write operations are deliberately not routed and follow in a later change with their own review) |
| 4 | **Decision**: remote-files model, OSE on the node (large) or the server OSE on the project mount (medium) | decision | remote files, language servers, extensions per computer | #457 |
| 5 | Authenticated relay of the OSE socket | medium | phone and second computer can connect | #458 |
| 6 | Extension install route and a data dir per computer (built, not yet compiled or run, see below) | small | extensions view, stable extensions across folders | #459 |
| 7 | Node file watching streamed to the server | medium | refresh for remote files, only if 4 is not the node OSE | #460 |

Client work that needs no server or node change: multi-root, a tasks runner over the terminals API, the debug UI, `$acceptConfigurationChanged`, reconnect, an SCM panel on the existing git relay, the settings storage design.

### Security of the IdeLaunch endpoint and token (as written by the server side)

- **Who can obtain it.** `GET /api/sessions/{id}/ide` is an owner route. On the local TCP listener it is behind the Host check, the Origin check and `Authorization: Bearer <server-token>` (D37 part 2; the token file is `<data dir>/server-token`, mode 0600); over the transport it is served to devices that passed the device gate (D37 part 1, FR-N7), without the token layer. A guest cannot reach it: the guest door serves only guest routes on a separate listener.
- **What an admitted device is trusted with.** D37: "full authority for now, scope later", comparable to an SSH key (per-device scope: #366). The IdeLaunch JSON hands such a device the OSE connection token, which gives code execution (terminal, extension host) as the server user. That is within the D37 trust level and not a new hole, but it is a second credential path beside the server token (#461).
- **The token is not in argv (fixed, #461).** The OSE gets `--connection-token-file <path>` (mode 0600 under a 0700 `tokens` dir, deleted with the process), so `ps` no longer shows it. Unverified with the real OSE: that it honours the file and how it treats a trailing newline (none is written).
- **Loopback only.** The OSE listens on `127.0.0.1`; any local process on the server machine can reach the port but needs the 64-hex token for the handshake. Whether the OSE answers `GET /version` and the static workbench without a token is **unknown** (the live test ran with `--without-connection-token`).
- **Can a remote device use the endpoint?** Not as built (blocker 2).
- **Token properties.** Random (two v4 UUIDs), per folder, stable for the life of the child, returned on every `GET /ide` for that folder, plain text in the JSON (`Debug` redacts it); no rotation, no per-client token, no revocation short of killing the child; the child's stdout and stderr go to null.
- **Transport.** Plain TCP on loopback, no TLS.
- **The VS Code Web path is weaker:** `--without-connection-token`; its protection is the proxy's own auth (`ember/proxy/dpx/auth`), not reviewed here (unknown).

### Task 6, built (#459, FR-W4)

- Data dir: `OseManager` has a data root, `EMBER_OSE_DATA_DIR`, else `<EMBER_DATA_DIR or ~/.ember>/ose-data`. Every OSE start and every install passes `--server-data-dir <root>/<computer id>` and `--extensions-dir <root>/<computer id>/extensions`. The directories are created with mode 0700. The computer id is one path segment of `[A-Za-z0-9._-]`, never empty, never containing `..`, so a request value cannot leave the root. Only the computer `local` exists for the OSE today (`connection(folder)` is `connection_for("local", folder)`).
- One data dir per computer, shared by that computer's folders, so an extension is installed once. **Unknown:** several OSE processes on one data dir (no lock or race documentation, no test). Nothing here claims it is safe.
- Route: `POST /api/computers/{id}/ide/extensions` with `{"id":"publisher.name","version":"1.2.3"}` (version optional). Id (letters, digits and `-` in both parts, uppercase allowed) and version are matched against fixed patterns; no VSIX paths and no URLs. The binary runs without a shell, with a 120 s timeout (the child and its process group are killed), output capped at 16 KiB. Answers: 200, 400 (bad id or a non-local computer), 404, 409 (an install is running on that computer), 502 (the command failed, with its output), 503 (no binary), 500. It sits in the token-protected router; the guest door does not serve it.
- Environment: the OSE start and the install child run with a cleared environment (`HOME`, `PATH`, `LANG`, `LC_ALL`, `TMPDIR`, `USER` only). Proxy variables, `SSL_CERT_FILE`, `SHELL`, `TERM`, `XDG_*` and `SSH_AUTH_SOCK` do not reach the OSE or its extension host: downloads behind a proxy are unverified and may fail. Opt-in pass-through is #471.
- CLI flags: `--server-data-dir`, `--extensions-dir` and `--install-extension` are documented in `ember/ose/README.md` and used by `ember/ose/smoke.sh`. **Unverified:** `--install-extension publisher.name@version` (smoke.sh installs without a version), and that a running OSE loads `<data dir>/extensions` (the start passes `--extensions-dir` too, so it does not depend on the default). `--list-extensions` is not documented anywhere in the repository, so there is no list route. **Known gap:** an install from Open VSX needs `extensions.verifySignature` off in the data dir's settings (smoke.sh writes it for the Machine and User scopes). The route does not set it and does not verify signatures, so installed extension code later runs in the editor's extension host. The route is owner/admin only (D37, token-protected, not on the guest door). The OSE child gets a cleared environment with `HOME`, `PATH`, `LANG`, `LC_ALL`, `TMPDIR` and `USER` only.
- Not compiled and not run against a real OSE. The tests use a fake binary.

### Unknowns to resolve before relying on this

Whether `dpx-ose-server` answers `GET /version` and static files without the token; whether a registered node's computer id ever equals an `EMBER_IDE_COMPUTERS` name; whether the project mount exists when the editor opens a folder (it is made only for agent session starts); which OSE IPC channels exist for search, extension management and user-data sync at the pinned commit (the connection layer uses exactly three); how two OSE processes behave when they share the default server data dir; the FR-W5b ide-gateway (mobile) is design only and a possible carrier for task 5.

## Notes

- The fallback `Open in VS Code` (`ember/editor/src/fallback.rs`, the proxy-wrapped web workbench in `ember/proxy`) is the only path today where every row works, at the price of a webview; it stays the fallback, not the design (`INTENT.md` D9a).
- Version risk: proxy ids and signatures change per Code-OSS release; each typed call is checked at the pinned commit (CI job `editor-conn-pin`).
- Unknown (not inspected): the contents of a built OSE archive; whether settings sync, remote tunnel or the webview host work in a built OSE.
- The umbrella issue is #32. Rows are not closed by this file: each issue states its own done condition.
