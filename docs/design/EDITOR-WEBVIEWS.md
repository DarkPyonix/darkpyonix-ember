# Extension webviews in the Ember 에디터 window

Owner decision 2026-11-13 (`INTENT.md` D44, D9a amendment; `SPEC.md` FR-E5, FR-E8): extension webview panels and views, custom editors and HTML/JS/widget notebook outputs run in a platform webview inside the editor window, each contained to its panel. The workbench stays dioxus-compose. This note is the Ember side and the requirements for the toolkit node; the node itself is compose-rust's (p1A, `.scratch/leader/native-webview-plan.md`).

## Per-OS host

| OS | Webview |
|---|---|
| macOS, iOS | WKWebView |
| Windows | WebView2 |
| Linux | WebKitGTK |
| Android | Android WebView |

The API Ember sees is the same on every OS (D25); only the host crate differs.

## What Ember needs from the host node

1. **Many per window.** One webview per extension webview panel, per custom editor tab and per notebook output area; several alive at once.
2. **Placement.** Each webview has its own rect inside the dioxus-compose layout, follows its panel on resize, scroll and window zoom, and has a z-order: app menus, sheets and drawers draw above it.
3. **Lifecycle.** Create, show, hide without destroying (Code-OSS `retainContextWhenHidden` keeps the page and its state while hidden), destroy; a load/ready event.
4. **Origin and isolation.** A unique secure origin per webview (Code-OSS: `vscode-webview://<uuid>`), so webviews cannot read each other or the host. The extension's HTML is loaded as given and its own CSP `<meta>` applies.
5. **Local resources.** A custom scheme handler answered by the host from Rust (request -> bytes + MIME type), limited to the webview's `localResourceRoots`; anything outside is refused (Code-OSS `vscode-resource` / `vscode-cdn`, `asWebviewUri`).
6. **Message channel.** `postMessage` both ways; JSON values plus `ArrayBuffer` / `Uint8Array`; ordered per webview; messages sent before the page is ready are queued and delivered after load.
7. **Script before page scripts.** Inject the `acquireVsCodeApi()` shim (`postMessage`, `getState`, `setState`) and the theme's CSS variables before any page script runs; update the variables live on theme change.
8. **Focus and keyboard.** Focus in/out events; keys go to the focused webview, while the app's shortcuts (quick open, palette, tab switching) still reach the window (Code-OSS forwards keydown from the webview to the workbench).
9. **Find in page** for the Code-OSS find widget over a webview; **zoom** follows the window's zoom level.
10. **No default context menu** and **no top-level navigation**: links open through the host, which decides.
11. **Same API on every OS**; WKWebView first is fine.

## Ember side

- **Bridge.** Decode `MainThreadWebviews`, `MainThreadWebviewPanels`, `MainThreadWebviewViews`, `MainThreadCustomEditors` and the notebook renderer messages from the extension host (`ember/editor-conn`, `ember/editor`), keep one host webview per handle, and route `$postMessage` / `onDidReceiveMessage` through the channel above.
- **Composition.** The editor window places webviews where Code-OSS puts them: an editor tab (webview panel, custom editor), a view in a tool window (webview view), an output area in a notebook cell.
- **Theme.** Code-OSS's `--vscode-*` CSS variables are generated from Ember's light and dark tokens (DESIGN.md), so extension pages match the window.
- **CI check.** `check-no-webview.sh` (NFR-L2) keeps forbidding webviews in the launcher and conversation UI and allows exactly the editor window's webview host module.

## Ember side: what is built (#823)

Built without the host node, behind a trait, and tested with a fake host (`fr_e8_*`):

- **Decoding** (`ember/editor-conn/src/webview.rs`): every call of `MainThreadWebviews`, `MainThreadWebviewPanels`, `MainThreadWebviewViews` and `MainThreadCustomEditors` at the pinned commit (the panel's dispose is `$disposeWebview`), and the calls back: `$onMessage`, `$onMissingCsp`, `$onDidChangeWebviewPanelViewStates`, `$onDidDisposeWebviewPanel`, `$deserializeWebviewPanel`, `$resolveWebviewView`, `$onDidChangeWebviewViewVisibility`, `$disposeWebviewView`, `$createCustomDocument`, `$resolveCustomEditor`, `$disposeCustomDocument`. A message stays a JSON string plus side buffers, as on the wire.
- **Model** (`ember/editor/src/webview/mod.rs`): one record per handle (kind, title, HTML, content and panel options, state, visible, active, position, ready); messages queue until the page is ready and keep their order; view states go out for every panel at once; a view's state survives close and resolve; a failed resolve shows Code-OSS's error page in Korean; `command:` links run only when `enableCommandUris` allows them. `SessionCore` routes the four actors to it (`Input::Webview`, `SessionUpdate::Webview`).
- **Resources** (`webview/resource.rs`): the `asWebviewUri` form decoded, `.` and `..` resolved, and a file served only strictly under `localResourceRoots`; the bytes come from the remote filesystem channel. The URL form and the page origin are the host's (`WebviewHost::resource_base`, `WebviewHost::origin`): a custom scheme (`vscode-resource://`, `vscode-webview://<uuid>`) on macOS and Linux, where WKWebView and WebKitGTK cannot intercept `https://`, and `https://` on Windows and Android. The extension host always writes Code-OSS's form, so the driver rewrites the page's HTML, its CSP source and messages into the host's form and turns request URLs back before the session checks them.
- **Theme** (`webview/theme.rs`, `webview/shim.rs`): the `--vscode-*` variables from Ember's light and dark tokens, covering every variable the webview's default stylesheet reads; the init script with `acquireVsCodeApi()` and the saved state.
- **Host** (`webview/host.rs`): the `WebviewHost` trait the node implements (with find, find next/previous/stop and their match counts, developer tools, and the app chords the page did not handle; the channel global's name is `shim::IPC_GLOBAL`, `__emberWebviewHost`), the `WebviewDriver` that applies updates and drops hidden pages without `retainContextWhenHidden`, the `WebviewPanes` placeholders ("웹뷰를 표시하는 중입니다"), and `FakeHost`.

Not built yet: the host node and drawing the panes in the window (compose-rust p1A), the window's custom editor "Open With" and webview view places, notebook renderer messages, custom document edits and dirty state, and the `check-no-webview.sh` exception for the host module.

## Open

- The compose-rust node and its crate (amending compose-rust INTENT C1 through an opt-in crate waits for the owner's OK).
- Service workers inside webviews (some notebook renderers use them): to be measured per OS.
- Remote webview resources when the folder is on another computer: served through the OSE file channel.
