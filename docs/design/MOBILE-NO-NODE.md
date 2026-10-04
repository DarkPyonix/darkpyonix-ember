# MOBILE-NO-NODE.md: the IDE on Android and iOS without Node (`FR-W5`)

> **Status: planned, design only, 2026-10-03; build tracked in #80.** [user] "이거 기능 설계는 미리 해놔. 후순위로 배치하는건
> 이해하는데 기능은 미리 설계해둬야 해." `FR-W5` stays outside the 10-18 deadline (`PROJECT.md`,
> *Excluded from this deadline*); this document fixes what will be built when it is scheduled. No
> code exists for it. Proposed SPEC rows are in §9 and are applied to `docs/SPEC.md` §W.
>
> **Marking.** **[user]**: from the user. **[provisional]**: team proposal. **[verified]**:
> checked against the cited source on 2026-10-03 (VS Code sources read at `microsoft/vscode` `main`).
> **[unverified]**: believed true, not checked; each one is listed again at the end of §10.

---

## 1. Problem

`FR-W5` [user]: on Android and iOS the IDE window must work with no Node anywhere on the device,
"through a `serve-web`-compatible Rust backend, or directly through web APIs with no backend".
`INTENT.md` D10 already draws the consequence: without Node there is no Node extension host, so
only extensions with a web build run locally; extensions that need Node need a VS Code server on a
computer. E4 forbids the obvious shortcut of writing our own extension host.

What this document has to decide:

1. Which of the two named routes (A: a `serve-web`-compatible Rust backend on the phone; B: the
   `vscode.dev` model, no backend) is worth building, or whether neither is the main path.
2. How the IDE fits the product. Ember is "one AI moving between computers" (`INTENT.md`); the
   phone is almost always a **client of other computers** (the main server and the project's
   computers), not a place where code runs. So a third option is considered:
   **C: the Ember editor core on the phone, talking to a remote ember node / OSE server over the
   ember transport when online, with a local-only mode offline.**
3. What happens offline, and what the app stores allow.

## 2. Constraints

| Source | Constraint | Effect on this design |
| ------ | ---------- | --------------------- |
| E1, `NFR-L2` | Launcher and conversation screens never contain a webview. | Any VS Code Web mode lives only in the IDE window. The gateway (§6) is a library, not a screen. |
| E4, `IMPLEMENTATION.md` §1 | VS Code is wrapped, never modified; the Extension Host is never reimplemented. | Rules out a "fake" remote extension host (§4.1). Only VS Code's own web-worker extension host may run on the phone. |
| `IMPLEMENTATION.md` §2 | Static workbench serving is substitutable; the Extension Host is not. | The phone may serve workbench bytes itself; it must not stand in for the server's extension host. |
| D9, 10-03 decisions [user, relayed by the leader; not yet written into `INTENT.md`] | Ember's own IDE is a **dioxus-compose editor core**: no webview, **no JS engine**. The VS Code target is VS Code Web wrapped by `ember/proxy/`. Default runtime is **OSE** (Code-OSS + Open VSX). | The editor core cannot run any VS Code extension itself. VS Code compatibility on the phone comes only from VS Code Web. |
| D10 | Marketplace follows the runtime: Open VSX for OSE. | Offline web extensions on the phone come from Open VSX. |
| E3, D3 | Conversations, agents and credentials live on the main server. | The phone never needs to run agents. Its IDE is for looking and editing. |
| `FR-N1`, `FR-N5` | All traffic goes through one transport interface (iroh behind it). | The IDE window reaches remote computers through that interface, not through a separate tunnel. |
| `FR-N4`, `ember/proxy/README.md` | VS Code Web webviews need service workers, so they need a secure context. | On a phone the IDE window must load from an origin the platform webview treats as secure (§5). |

## 3. What VS Code actually requires: sources

### 3.1 `serve-web` is a Rust launcher in front of a Node server [verified]

`code serve-web` is implemented in the Rust CLI (`cli/src/commands/serve_web.rs`). It downloads
the VS Code **server** build for a commit, unzips it, starts its Node entry point with
`--socket-path` (`start_version`), and then only forwards HTTP and WebSocket requests to it
(`forward_http_req_to_server`, `forward_ws_req_to_server`). Everything the browser talks to is the
Node server. So "a `serve-web`-compatible Rust backend" means reimplementing what the Node server
answers, not what the Rust CLI does.

### 3.2 What the Node server answers [verified]

HTTP (`src/vs/server/node/webClientServer.ts`, `remoteExtensionHostAgentServer.ts`):

| Route | Purpose | Substitutable? |
| ----- | ------- | -------------- |
| `/` | Workbench HTML with the embedder config (`remoteAuthority`, workspace, CSP with a nonce) | Yes: template rendering |
| `/stable-<commit>/static/...` | Workbench JS/CSS/fonts | Yes: static files (`IMPLEMENTATION.md` §2) |
| `/web-extension-resource/...` | Proxy for web-extension files from the gallery | Yes: HTTP proxy |
| `/vscode-remote-resource` | Files from the remote filesystem by URL (images, fonts in extensions) | Yes: file read |
| `/version`, `/callback`, `/delay-shutdown` | Small endpoints | Yes |

WebSocket: the **remote agent protocol**:

- Framing: `PersistentProtocol` in `src/vs/base/parts/ipc/common/ipc.net.ts`, a 13-byte header
  (`ProtocolConstants.HeaderLength = 13`), message types `Regular, Control, Ack, Disconnect,
  ReplayRequest, Pause, Resume, KeepAlive`, with acknowledgement, replay and reconnection by
  `reconnectionToken`.
- Handshake (`remoteExtensionHostAgentServer.ts` `_handleWebSocketConnection`): `auth` → server
  `sign` → `connectionType` carrying the **renderer's commit**; a commit mismatch is refused
  ("Client refused: version mismatch"). Then one of three connection types:
  `Management`, `ExtensionHost`, `Tunnel`.
- On the management connection, RPC channels with the binary IPC encoding of
  `src/vs/base/parts/ipc/common/ipc.ts` (request types 100–103, response types 200–204, a tagged
  value serialisation). Channels registered in `src/vs/server/node/serverServices.ts` include
  `remoteextensionsenvironment`, `remoteFilesystem` (`stat, realpath, readdir, readFile,
  writeFile, open, close, read, write, mkdir, delete, rename, copy, cloneFile, watch, unwatch`;
  `diskFileSystemProviderClient.ts`), `remoteterminal`, `extensions`, `logger`, `telemetry`,
  `request`, `userDataProfiles`, `mcpManagement`, the remote extensions scanner, and more.
  These are internal, unversioned except by commit, and change with releases.

### 3.3 The remote extension host is not optional once there is a remote [verified]

In the web workbench, `AbstractExtensionService._startExtensionHostsIfNecessary` always pushes a
`RemoteRunningLocation`, and `browser/extensionService.ts` `createExtensionHost` creates a
`RemoteExtensionHost` whenever a remote agent connection exists. So a backend that declares a
`remoteAuthority` must also accept an `ExtensionHost` connection and speak the extension-host RPC
(the "massive, typed RPC protocol" of `IMPLEMENTATION.md` §1), or the workbench runs with a broken
remote extension host. What the workbench shows when that connection is refused was **not tested**
[unverified].

### 3.4 The browser-only model [verified unless marked]

- Extension host kinds: `LocalProcess`, `LocalWebWorker`, `Remote`
  (`extensionHostKind.ts`). Without a remote, all extensions run in `LocalWebWorker`.
- Web extensions declare a `browser` entry instead of `main`; they run "in a Browser WebWorker
  environment"; "creating child processes or running executables is not possible"; files go
  through `vscode.workspace.fs`. An extension may have both `main` and `browser`
  (code.visualstudio.com/api/extension-guides/web-extensions).
- `vscode.dev` opens local folders through the File System Access API ("Edge and Chrome today
  support the File System API"), via `HTMLFileSystemProvider` keeping `FileSystemDirectoryHandle`s
  in IndexedDB (`src/vs/platform/files/browser/htmlFileSystemProvider.ts`); remote repositories
  through the GitHub Repositories extension, a virtual file system; no terminal, no debugger
  (code.visualstudio.com/docs/setup/vscode-web).
- The embedder API `IWorkbenchConstructionOptions` (`src/vs/workbench/browser/web.api.ts`) lets
  the page that boots the workbench set `remoteAuthority` (or leave it out), a `workspaceProvider`,
  `webviewEndpoint`, `additionalBuiltinExtensions`, `messagePorts` (a `MessagePort` per extension
  ID), `configurationDefaults` and more: **configuration, not a source patch**, so it stays within
  E4.
- Precedent for "static workbench + a filesystem served by our own process":
  `@vscode/test-web` serves the workbench and mounts a local folder as a virtual workspace (scheme
  `vscode-test-web`) "backed by a file system provider that gets the file/folder data from the local
  disk" through a built-in extension (github.com/microsoft/vscode-test-web).

### 3.5 Platform facts

| Fact | Status / source |
| ---- | --------------- |
| iOS: `WKWebView` runs JavaScriptCore out of process **with JIT**; an app's own in-process JavaScriptCore runs **without** JIT; Lockdown Mode disables JIT in WebKit. | Community sources (HN 40726948; 9to5Mac on Lockdown Mode) [unverified against Apple docs]. Consequence: VS Code Web in `WKWebView` is fast enough; an embedded JS engine in our own process would not be, and the 10-03 decision rules one out anyway. |
| iOS: service workers in `WKWebView` only with **App-Bound Domains** (`WKAppBoundDomains` in Info.plist, up to 10 domains, `limitsNavigationsToAppBoundDomains = true`; navigation elsewhere fails). | webkit.org/blog/10882/app-bound-domains [verified for http(s) domains]. Whether `localhost` / `127.0.0.1` may be an app-bound domain, and whether a `WKURLSchemeHandler` custom scheme can host a service worker: [unverified]. |
| iOS / Safari / `WKWebView`: no File System Access pickers (`showDirectoryPicker`); only the Origin Private File System (iOS 15.2+). | MDN / caniuse-derived sources [verified as of public data; Apple has not announced pickers]. So the `vscode.dev` "open local folder" path does not exist on iOS. |
| iOS: an App Store app cannot spawn subprocesses (no `fork`/`posix_spawn` of arbitrary binaries). | [unverified: widely known; no Apple doc read]. No local terminal, no local language servers. |
| iOS: background execution only for listed purposes (2.5.4). The app is suspended in the background. | App Review Guidelines 2.5.4 [verified]. The IDE window and gateway only need to run in the foreground. |
| iOS App Review 2.5.2: no downloading or executing code "which introduces or changes features or functionality of the app"; the exception is now only for **educational** apps. | Guidelines [verified 2026-10-03]. |
| iOS App Review 4.7: apps may offer "HTML5 and JavaScript mini apps … and plug-ins" not embedded in the binary; 4.7.1 content filtering/reporting; **4.7.2 "may not extend or expose native platform APIs or technologies to the software without prior permission from Apple"**; 4.7.4 an index of the software offered; 4.7.5 age gating. 2.5.6: web browsing must use WebKit. | Guidelines [verified]. Whether an Open VSX extension gallery inside the app passes as 4.7 "plug-ins", and whether our filesystem bridge counts as "exposing native APIs" (4.7.2): **[unverified: a review-risk judgement, not a fact]**. |
| Android WebView: origins served by `androidx.webkit.WebViewAssetLoader` (e.g. `https://appassets.androidplatform.net`) are secure contexts; a custom `shouldInterceptRequest` on a non-https origin is not, so service workers fail there. | developer.android.com `WebViewAssetLoader`; tauri-apps/wry #1709 [verified as reported]. `http://localhost` is also a secure context in Chromium [verified for Chrome; for WebView unverified]. |
| Android WebView: File System Access (`showDirectoryPicker` etc.) enabled in M132; the app must implement `WebChromeClient#onShowFileChooser`; content-URI files have no atomic writes/renames and large folders are slow. | blink-dev "Intent to Ship: File System Access on Android and WebView" [verified]. |
| Google Play: no downloading executable code (dex, JAR, .so) outside Play; "does not apply to code that runs in a virtual machine or an interpreter … (such as JavaScript in a webview or browser)". | Play Console Help, Device and Network Abuse [verified]. Web extensions in a WebView are allowed. |

---

## 4. The three approaches

### 4.1 A: a `serve-web`-compatible Rust backend on the phone

The Rust process in the app answers the Node server's HTTP routes and remote agent protocol (§3.2)
against the app's sandbox, and the IDE window loads VS Code Web from it as if from `serve-web`.

Must implement: the HTTP routes (easy); `PersistentProtocol` framing with ack/replay/reconnect;
the auth/sign/connectionType handshake **pinned to the exact workbench commit**; the IPC
serialisation; and at least `remoteextensionsenvironment`, `remoteFilesystem` (with `watch`),
`extensions`, `logger`, `request`, `userDataProfiles`, plus an answer to the **ExtensionHost
connection** that the workbench always opens (§3.3).

Cannot do: run any Node extension (no Node, and E4 forbids a substitute host). On iOS it also
cannot run `remoteterminal` (no subprocesses). So every extension still runs in the web worker,
**exactly as in B**, and the only thing A adds over B is "files are reached through
`remoteFilesystem` instead of a web-side provider", paid for with an internal, per-commit protocol
re-implemented in Rust and re-verified on every OSE release. The ExtensionHost connection is a
dead end: refuse it and the workbench runs degraded [unverified how], accept it and we are writing
an extension host.

### 4.2 B: `vscode.dev` model, no backend

The IDE window boots a bundled, pinned OSE workbench with no `remoteAuthority`; every extension runs
in `LocalWebWorker`. Files come from a file-system provider inside the workbench.

On a phone the `vscode.dev` way of opening a local folder **does not exist**: iOS has no File
System Access pickers at all, and Android WebView's (M132+) works on content URIs without atomic
writes. So on mobile "no backend" in practice means: the app's own Rust code serves the static
assets and exposes the working copy to a **built-in web extension (`ember-fs`)** that registers a
`FileSystemProvider`: the `@vscode/test-web` pattern (§3.4), using only the embedder API and the
public extension API. Remote repositories work as on `vscode.dev`, through virtual-FS web extensions.

Extensions: only those with a `browser` entry (themes, grammars, snippets and declarative
extensions run unmodified; language support only where a web build exists). No terminal, no
debugger, no tasks, no Node language servers.

### 4.3 C: Ember editor core on the phone; remote when online, local-only offline

The phone runs Ember's **dioxus-compose editor core** (no webview, no JS engine). Online, it opens
files on the session's current computer through that computer's **ember node** over the ember
transport (`FR-X1` file operations: read, write, list, search, watch), and the full VS Code target
is still one tap away: "Open IDE → VS Code" loads **the remote computer's `serve-web` (OSE or VSC)**
in the IDE window through the in-app gateway (§6). Node runs only on the remote computer, so the
phone has no Node and every extension works: it is the `Remote` extension host kind VS Code
already ships (`IMPLEMENTATION.md` §1). Offline, the editor core edits a **local working copy** in
the app sandbox and syncs it back later.

Language features in the editor core: it cannot run VS Code extensions. Online, it can show
diagnostics/hover/completion from **language servers that the remote ember node runs** (LSP over a
transport stream) [provisional; needs an `FR-X` addition]. Offline it has syntax highlighting only.

### 4.4 Comparison

| | **A**: `serve-web`-compatible Rust backend on phone | **B**: `vscode.dev` model, no backend | **C**: editor core; remote online, local offline |
| - | - | - | - |
| What runs on the phone | VS Code Web (WebView) + Rust server speaking the remote agent protocol | VS Code Web (WebView) + Rust static/file gateway + `ember-fs` web extension | dioxus-compose editor core (native). VS Code Web in the IDE window only when the user picks the VS Code target |
| Extension compatibility | Web extensions only (all run in the web worker). Node extensions: none. | Web extensions only (`browser` entry). Node extensions: none. | **Online via VS Code target: all extensions of the runtime's marketplace** (remote Node host). Editor core: none; LSP from the remote [provisional]. Offline: none. |
| Offline | Yes, local working copy | Yes, local working copy | Yes, editor core on a local working copy (no extensions) |
| Terminal / debug / tasks | No (no subprocesses on iOS; Android possible but out of scope) | No | Online: yes, on the remote computer (VS Code target, or `FR-X1` PTY) |
| Work required | **Largest.** HTTP routes + `PersistentProtocol` + handshake + IPC codec + ~6–10 channels + an ExtensionHost-connection answer; redone per OSE commit. | Medium. Bundle pinned OSE web build; gateway (static + webview origin + file API); `ember-fs` extension; offline extension install from Open VSX. | Online VS Code: small (gateway + transport; reuses `FR-W1`/`FR-N4`). Editor core: depends on §E (large, already planned). Offline sync: medium. |
| Upgrade risk | High: internal protocol, commit-pinned handshake (§3.2) | Low: embedder API + public extension API | Low for the VS Code path (stock `serve-web`); editor core is our own code |
| E4 fit | **Poor**: the ExtensionHost connection forces either a broken host or a substitute host | Good | Good |
| E1 fit | IDE window only | IDE window only | Best: editor core has no webview; WebView only in the VS Code target |
| iOS: JIT | Fine (WKWebView JIT) [unverified as above] | Fine | Editor core needs none; VS Code target fine |
| iOS: WKWebView service workers | Needs App-Bound Domains incl. a loopback origin [unverified] | Same | Same, only for the VS Code target |
| iOS: background | Server suspended in background; fine for a foreground editor | Same | Same; transport reconnect on foreground ≤ 1 s (`NFR-N1`) |
| iOS: app review | 2.5.2/4.7 risk from downloaded extension code + 4.7.2 risk (native file bridge to third-party code) | **Same risk** | **Lowest**: offline mode downloads no code; VS Code target loads the user's own remote server, like a browser |
| Android | WebView + `WebViewAssetLoader` or loopback; service workers OK on a secure origin | Same; File System Access M132+ optional | Same for the VS Code target |
| Play policy | JS in WebView is allowed | Allowed | Allowed |
| Fits "one AI moving between computers" | Weak: makes the phone a server | Medium: the phone as a standalone editor | **Strong**: the phone is a client of the computers it is moving between |

## 5. Recommendation [provisional]

**Build C as the main path, B as the offline VS Code mode, and do not build A.**

1. **C online first.** The phone is a client: "Open IDE" on a phone opens the project on the
   session's current computer. The VS Code target loads that computer's own `serve-web` through
   the in-app gateway over the transport. This already meets `FR-W5`'s acceptance criterion (no
   Node on the device) while keeping **every** extension working, because the extension host is
   VS Code's own `Remote` host on the computer. The Ember-IDE target uses the editor core over
   ember node file operations. Both reuse what M5 and M7 build anyway (transport, `FR-W1`, `FR-N4`).
2. **C offline: editor core on a local working copy.** No code is downloaded, so it is safe on iOS.
   Changes sync back with base-hash conflict detection (the same content-hash idea as `INTENT.md`
   Q4).
3. **B as an opt-in offline VS Code mode**, Android first. A bundled, pinned OSE web build; all
   extensions in the web worker; the same local working copy exposed through `ember-fs`. On iOS it
   ships behind a flag only after the App Review question (§10 Q-M3) is answered.
4. **A is rejected** for the phone. It costs the most, tracks an internal protocol per commit, and
   buys nothing over B: no Node means every extension runs in the web worker in both. Its one
   structural requirement (answering the always-opened ExtensionHost connection, §3.3) collides
   with E4. *Revisit if:* VS Code publishes a stable remote-server protocol, or the leader wants a
   Rust `serve-web` replacement on **computers** (a different question; there Node is available and
   the extension host still is VS Code's).

How this reads against the user's `FR-W5` wording: "directly through web APIs with no backend" is
B, kept. "A `serve-web`-compatible Rust backend" is satisfied in the form that matters (the phone
talks to a real `serve-web` on a computer through a Rust gateway) rather than by re-implementing
`serve-web` on the phone. **This reinterpretation needs the user's confirmation (Q-M1).**

## 6. Architecture

```
┌──────────────────────────── ember app (Android / iOS, one process) ────────────────────────────┐
│                                                                                                │
│  dioxus-compose (no webview, E1)                    IDE window (the only webview)              │
│  ┌──────────────────────────────┐                   ┌─────────────────────────────────────┐   │
│  │ launcher · conversations     │  Open IDE ───────▶│ WKWebView / Android WebView         │   │
│  │ ─────────────────────────────│                   │  VS Code Web workbench (OSE/VSC)    │   │
│  │ editor core (Ember IDE)      │                   │  + ember/proxy/ overlay (FR-W2)       │   │
│  │  ├ remote doc ─┐             │                   │  + ember-fs web extension (mode B)  │   │
│  │  └ local doc ─┐│             │                   └──────────────┬──────────────────────┘   │
│  └───────────────┼┼─────────────┘                                  │ http(s)+ws, loopback     │
│                  ││                                                ▼  secure origin           │
│   ┌──────────────┘│                       ┌──────────────────────────────────────────────┐   │
│   │ working-copy  │                       │ ide-gateway (Rust)                            │   │
│   │ store (sandbox│◀──── /__fs/* ─────────│  • mode C-online: reverse proxy HTTP+WS ─────┼─┐ │
│   │  files, base  │      (mode B)         │  • mode B: static OSE assets, webview origin, │ │ │
│   │  hashes, sync)│                       │    /__fs/* over the working copy              │ │ │
│   └──────┬────────┘                       │  • token per launch, binds loopback only      │ │ │
│          │ sync when online               └──────────────────────────────────────────────┘ │ │
│          ▼                                                                                  │ │
│   ┌────────────────────────── transport crate (FR-N5; iroh inside) ─────────────────────────┴─┐│
│   └───────────────────────────────────────┬──────────────────────────────────────────────────┘│
└───────────────────────────────────────────┼───────────────────────────────────────────────────┘
                                            │ QUIC, direct or via darkpyonix.dev relay
              ┌─────────────────────────────┴──────────────────────────────┐
              ▼                                                            ▼
   ember server (main server)                                 computer: ember node
   sessions · transcripts · accounts                          • file ops / watch / PTY (FR-X1)
   which computer is "current" for the session                • stream to local serve-web (OSE/VSC)
   (only control traffic for the IDE)                           ── Node extension host lives HERE
                                                              • language servers for the editor
                                                                core (LSP stream) [provisional]
```

Notes:

- **One gateway for every webview mode.** `WKURLSchemeHandler` cannot carry WebSockets and
  Android's request interception cannot either, so a loopback HTTP server in the app process is the
  single way in for VS Code Web; it is foreground-only, binds `127.0.0.1`, and requires a per-launch
  token in a cookie or query (like `serve-web`'s connection token). On Android the static part may
  be served through `WebViewAssetLoader` instead [provisional: whichever passes the service-worker
  test].
- **Webview isolation.** VS Code Web loads extension webviews from `webviewEndpoint`, a separate
  origin. The gateway serves it on a second loopback origin (e.g. `127.0.0.1` vs `localhost`), both
  secure contexts; on iOS both must be app-bound domains [unverified].
- **Which computer.** The gateway asks the ember server for the session's current computer, then
  opens a transport stream to that computer's ember node, which connects to its local `serve-web`.
  When the session moves (`INTENT.md` Q3) the IDE window either stays or is offered a reload; that
  decision is Q3's, not this document's.
- **No Node on the phone, in every mode.** The only JavaScript is the workbench and web extensions,
  running in the platform WebView's engine.

Illustrative `ember-fs` boot (mode B), embedder API only:

```js
// workbench bootstrap page served by ide-gateway (illustrative)
create(document.body, {
  // no remoteAuthority: every extension runs in the web-worker host
  workspaceProvider: { workspace: { folderUri: URI.parse('ember-wc://local/<project>') }, open: … },
  additionalBuiltinExtensions: [URI.parse(`${origin}/__builtin/ember-fs`)],
  webviewEndpoint: `${webviewOrigin}/stable-${commit}/out/vs/workbench/contrib/webview/browser/pre`,
  configurationDefaults: { 'workbench.colorTheme': 'DarkPyonix Ember' },
});
```

## 7. Local working copy and sync (offline modes) [provisional]

- **Making a project available offline** copies a chosen subtree from the project's current computer
  (through ember node) into the app sandbox, recording for each file `(path, content hash, computer,
  time)`. Size limit and excluded paths (`node_modules`, build outputs, `.gitignore`d files) are
  configurable. Git-aware alternative: a pure-Rust Git implementation (e.g. `gix`) clones the
  repository from the computer [unverified that `gix` covers clone/fetch/commit/push on iOS].
- **Editing offline** writes only to the working copy and keeps each file's base hash.
- **Syncing** when online sends a change set to the computer that the copy came from (or the
  session's current one, by user choice). A file whose remote hash still equals the base hash is
  written; one that changed on both sides is **never overwritten**; it is shown as a conflict with
  a three-way diff in the editor core.
- The same working copy serves the editor core (mode C-offline) and VS Code Web (mode B), so the
  user can switch between them offline.

## 8. Phased plan (after the 10-18 deadline, as `PROJECT.md` already places `FR-W5`)

| Phase | Delivers | SPEC | Depends on |
| ----- | -------- | ---- | ---------- |
| **P0: now** | This document; three spikes, each a day or less, before any build: (1) WKWebView service worker on a loopback origin with App-Bound Domains, (2) Android WebView service worker on `WebViewAssetLoader` vs loopback, (3) pinned OSE web build booted with no `remoteAuthority` and a test-web-style FS extension, size measured. | (none) | nothing |
| **P1: phone as client, VS Code target** | ide-gateway in reverse-proxy mode; "Open IDE → VS Code" on a phone opens the current computer's `serve-web` over the transport; extension webviews render. | `FR-W5a`, `FR-W5b` | M5 (transport), M7 (`FR-W1`, `ember/proxy/` overlay) |
| **P2: phone as client, Ember IDE target** | Editor core opens/edits/saves remote files via ember node; conflict check by hash. | `FR-W5c` | §E editor core, M2 (ember node) |
| **P3: offline, editor core** | Working copy, offline editing, sync with conflicts. | `FR-W5d`, `FR-W5e` | P2 |
| **P4: offline VS Code (B), Android** | Bundled pinned OSE web build, `ember-fs`, web extensions from Open VSX, compatibility labels. | `FR-W5f`, `FR-W5g` | P3 working copy, OSE build pipeline (`FR-W4`) |
| **P5: B on iOS** | Same as P4 behind a flag, after the review-risk decision. | `FR-W5h` | Q-M3 answered |
| **P6: editor-core language features online** | LSP servers run by ember node, streamed to the editor core. | (new `FR-X` row when scheduled) | P2 |

## 9. Proposed SPEC rows

Applied to `docs/SPEC.md` §W directly under `FR-W5`. All **[provisional]** except where `[user]`.

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-W5a** | *[provisional, `docs/design/MOBILE-NO-NODE.md`]* On Android and iOS, "Open IDE → VS Code" opens the session's project on its **current computer**: the IDE window loads that computer's own `serve-web` (OSE or VSC) through the in-app gateway (`FR-W5b`) over the transport (`FR-N5`). Node runs only on the computer. | On a phone with no Node anywhere on the device: open a project, edit and save a file, and the change is on the computer's disk. An extension with only a `main` (Node) entry installed on that computer works. An extension webview renders on a real phone (`FR-N4`). |
| **FR-W5b** | *[provisional]* One Rust **ide-gateway** in the app process serves the IDE window from a loopback origin that the platform webview treats as a secure context, with a second secure origin for `webviewEndpoint`. It reverse-proxies HTTP and WebSocket over transport streams (mode `FR-W5a`) and serves static workbench assets and the working-copy file API (mode `FR-W5f`). | Binds loopback only and rejects requests without the per-launch token. `navigator.serviceWorker` exists in both origins on iOS and Android. No `iroh` type is imported outside the transport crate (`FR-N5`). Wi-Fi ↔ LTE switch keeps the workbench connected within `NFR-N1`'s stall bound. |
| **FR-W5c** | *[provisional]* On Android and iOS, "Open IDE → Ember IDE" opens files of the current computer in the dioxus-compose editor core through ember node file operations (`FR-X1`) over the transport. No webview, no JS engine. | Open, edit and save a remote file from a phone, over a direct path and over the relay. If the file changed on the computer since it was opened, saving reports a conflict instead of overwriting. `NFR-L2`'s webview check covers the editor core. |
| **FR-W5d** | *[provisional]* A project subtree can be made available offline as a **local working copy** in the app sandbox, recording each file's base content hash; the editor core edits it with no network. | In airplane mode: open, edit, save, create and delete files in the working copy; the app restarts with the edits intact. Excluded paths and the size limit are honoured. |
| **FR-W5e** | *[provisional]* When online, working-copy changes sync to a chosen computer of the project; a file changed on both sides is never overwritten. | Edit file X offline only on the phone and file Y on both sides: X is written to the computer; Y is shown as a conflict with a three-way diff, and neither side's content is lost. |
| **FR-W5f** | *[user: `FR-W5` "directly through web APIs with no backend"; form provisional]* Offline VS Code mode: the IDE window boots a bundled, pinned OSE web build with no `remoteAuthority`, so every extension runs in VS Code's own web-worker extension host; the working copy is exposed through a built-in `ember-fs` web extension. Configured only through the embedder API (`IWorkbenchConstructionOptions`); VS Code source is not patched (E4). | In airplane mode on Android: open the working copy, edit, save, and run a web extension (a theme, a grammar, and one language extension with a `browser` entry). The served bundle matches its pinned release (`FR-W2`'s diff check). No Node on the device. |
| **FR-W5g** | *[provisional]* In any no-Node mode, the extensions view says for each extension whether it runs on the phone (`browser` entry) or needs a computer, and extensions without a `browser` entry are not offered for local install. | Classification comes from the extension manifest (`browser`, `main`, `extensionKind`); a `main`-only extension shows "needs a computer" and a one-tap switch to `FR-W5a`. |
| **FR-W5h** | *[provisional]* On iOS, `FR-W5f` (downloaded extension code) ships only behind a flag, enabled after the App Review decision (`MOBILE-NO-NODE.md` Q-M3). `FR-W5a`–`FR-W5e` ship without it. | An iOS build with the flag off downloads and executes no extension code locally. |
| **NFR-W5a** | *[provisional: initial targets, to be adjusted once by the first measurement]* Mobile IDE responsiveness. | On a reference mid-range Android phone and a reference iPhone: `FR-W5a` warm open to editable ≤ 3 s p95 over a direct path; `FR-W5c` remote file open ≤ 1 s p95 for a 100 KB file; `FR-W5f` cold open ≤ 5 s p95; IDE-window RSS recorded per mode. |

Rejected, recorded in the SPEC as a note: a `serve-web`-protocol-compatible server on the phone
(approach A); reason in §5.

## 10. Open questions for the user

| ID | Question |
| -- | -------- |
| **Q-M1** | `FR-W5` names "a `serve-web`-compatible Rust backend". Is it acceptable that this is met by the phone talking to a **real `serve-web` on a computer through a Rust gateway** (`FR-W5a`), rather than re-implementing `serve-web` on the phone (A, rejected)? Or did you mean a Rust `serve-web` replacement **on computers**? |
| **Q-M2** | Offline: is the editor core on a local working copy enough (`FR-W5d`), or must the offline IDE be VS Code with extensions (`FR-W5f`) from the first offline release? |
| **Q-M3** | iOS and downloaded extensions: accept the App Review risk of an Open VSX gallery inside the app (4.7 index/reporting/age-gating duties, 4.7.2 "no native APIs to the software"), keep `FR-W5f` Android-only, or ship it only through EU alternative distribution? |
| **Q-M4** | Where does an offline working copy come from and go back to: the computer it was copied from, the session's current computer, or the main server holding a copy? (Interacts with `INTENT.md` Q1.) |
| **Q-M5** | Should the editor core get language features from language servers on the remote computer (P6), or stay a plain editor on phones with VS Code as the "smart" target? |

### Facts not verified (collected)

- WKWebView: whether `localhost`/`127.0.0.1` can be an app-bound domain and host service workers;
  whether a `WKURLSchemeHandler` custom scheme can. (Spike P0-1.)
- Android WebView treating `http://localhost` as a secure context (Chromium does). (Spike P0-2.)
- WKWebView JIT vs in-app JavaScriptCore without JIT: from community sources, not Apple docs.
- iOS App Store apps cannot spawn subprocesses: common knowledge, no Apple doc read.
- How the workbench behaves when the remote ExtensionHost connection is refused (§3.3).
- Size of a pinned OSE web build bundled in the app; Open VSX serving web-extension resources to an
  OSE workbench with no server (`/web-extension-resource` is normally proxied by the Node server).
- `gix` support for clone/commit/push on iOS.
- App Review outcome for an in-app extension gallery (judgement, not fact).
