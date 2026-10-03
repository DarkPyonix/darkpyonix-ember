# ARCHITECTURE.md: DarkPyonix Ember

> **Revised 2026-10-03.** §1 is new and follows `INTENT.md`'s main-server model. §2 onwards is the
> 09-22 material, unchanged in substance, and now describes **the IDE window only**. The whole
> DarkPyonix component map (ember, ash, hub, kernel) lives in `darkpyonix-core/docs/ARCHITECTURE.md`;
> this document must stay consistent with it.
>
> Items marked *[provisional]* are team proposals not yet confirmed by the user.
>
> **Names.** The main server runs **ember server**; each computer runs **ember node** (the execution
> daemon). These are the names shared with `darkpyonix-core/docs/ARCHITECTURE.md`. *[provisional]*

---

## 1. Topology, end to end

```
                         ┌─────────────────────────────── darkpyonix.dev ───────────────────────────────┐
                         │  hole-punch coordination + relay fallback (FR-N2) · sign-in                    │
                         └───────────────▲──────────────────────────▲───────────────────────────▲─────┘
                                         │                            │                             │
  ┌──────────────────────────┐   P2P    │   ┌────────────────────────┴───────────────────────┐   │   ┌──────────────────────────┐
  │ Client (desktop / phone)  │◀────────┴──▶│              MAIN SERVER                          │◀──┴──▶│ Computer A (e.g. Mac)       │
  │                            │  PR-1 push   │   (personal Raspberry Pi or Mac mini)              │  P2P   │  ember node (exec daemon)   │
  │  launcher + conversations  │             │                                                     │        │  - file ops, search,         │
  │  dioxus-compose, NO webview│             │  ┌───────────────┐  ┌───────────────────────┐  │        │    commands + PTY (FR-X1)    │
  │  (E1)                       │             │  │ Agent CLIs      │  │ Session store           │  │        │  - background jobs (FR-X4)   │
  │                            │             │  │ Claude Code ×n  │  │ transcripts (normalised │  │        │  - browser egress (FR-R1)    │
  │  [Open IDE] ───────┐       │             │  │ Codex ×n        │  │  + native session files)│  │        │  - VS Code server for the    │
  └────────────────────┼──────┘             │  │ Antigravity ×n  │  │ accounts · computers     │  │        │    IDE window (§2)            │
                       │                      │  │ OMP ×n          │  │ projects · schedules     │  │        └──────────────────────────┘
                       ▼                      │  └───────┬───────┘  └───────────────────────┘  │
  ┌──────────────────────────┐             │          │ tool calls (wrapped shell, D4)          │        ┌──────────────────────────┐
  │ IDE window                  │             │          ▼                                          │◀─────▶│ Computer B (e.g. Linux GPU) │
  │  Ember IDE: webview +        │             │  ┌───────────────────────────────────────────┐  │  P2P   │  ember node (exec daemon)   │
  │  VS Code Web via web/proxy/   │             │  │ Execution router: sends each tool action   │  │        └──────────────────────────┘
  │  - or VS Code / Gateway       │             │  │ to the session's CURRENT computer (FR-X3)  │  │
  │  (FR-L7, §W)                  │             │  └───────────────────────────────────────────┘  │
  └──────────────────────────┘             │  A2A broker (§T) · account/usage router (§U)       │
                                             │  remote-browser profiles (FR-R2) · MCP registry     │
                                             └─────────────────────────────────────────────────────┘
```

### 1.1 What lives where

| Place | Holds | Never holds |
| ----- | ----- | ----------- |
| **Main server** | Every agent CLI process; every transcript and the agents' native session files; accounts and credentials; projects, computers and assignments; A2A queues; schedules; browser profiles | Project source as a system of record (that lives on the computers) |
| **Computer** | Project files; the processes tools start (builds, tests, servers); **ember node** (the execution daemon); the VS Code server for an IDE window on that computer | Agent CLIs; transcripts; credentials for agent accounts |
| **Client** | A cache of what the main server last pushed, for instant cold start | Anything authoritative |
| **darkpyonix.dev** | Connection coordination and relay | Conversations, files, credentials |

### 1.2 How a tool call travels

1. A user message (or an A2A message, `FR-T3`) reaches a session on the main server.
2. The session's agent CLI, running headless on the main server (`FR-A2`), decides to read a file
   or run a command.
3. Ember's wrapping layer intercepts the action at the shell/tool boundary (D4; exact point per
   agent is Q6) and the execution router sends it to the session's current computer.
4. The computer's ember node performs it and streams the result back; the agent sees it as if it had
   run locally (`FR-X2`).
5. The normalised event (`FR-A3`) is stored and pushed to every attached client (`PR-1`).

Switching computers (`FR-X3`) changes only step 3's destination. *[provisional]* On a switch, the
agent is told the computer changed and which earlier file observations are no longer valid
(`FR-S7`).

**Kernel status.** To show a DarkPyonix kernel's run state and latest output in the conversation
view, ember server calls that computer's darkpyonix manager HTTP API over the tunnel
(`darkpyonix-core/docs/ARCHITECTURE.md` §6). The kernel stack knows nothing about conversations,
accounts or computer switching.

### 1.3 Repository layout

| Path | What |
| ---- | ---- |
| `crates/server/` | ember server (Rust) |
| `crates/node/` | ember node, the execution daemon (Rust) |
| `crates/transport/` | the transport interface and its iroh backend (FR-N5); see §1.5 |
| `crates/client/` | the client core below the dioxus-compose UI |
| `web/proxy/` | the IDE window wrapping layer (Python) |
| `crates/bridge/` | the IDE window bridge: message types and the `WebviewBridge` trait (`ember-bridge`, FR-B1–B4) |
| `extensions/` | editor extensions: `vscode-darkpyonix`, `vscode-darkpyonix-theme`, `intellij-darkpyonix` |

### 1.4 Relationship to `web/proxy/` today

`web/proxy/` (merged in #1) is a FastAPI service in front of `code serve-web`. Its parts map onto this
topology as follows (`INTENT.md` D13, *[provisional]*):

| `web/proxy/` part | Role in the target architecture |
| ------------- | ------------------------------- |
| `dpx/vscode/`, `static/overlay.*`, `static/frame.html`, `static/webview-kb.js` | The IDE window's wrapping layer (`FR-W2`), kept |
| `dpx/agents/` (Claude Code / Codex transcript parsers) | Kept and moved to the main server, where the transcripts now are |
| `dpx/auth/` | Login for the IDE window; superseded by device authentication (`FR-N3`) once M5 lands |
| `static/home.html`, `dpx/home/` | Transitional web home; replaced by the native client (E1) |
| `dpx/hub/` (per-computer connectors reporting local transcripts) | Transitional; replaced by the main server holding transcripts (E3) |


### 1.5 Connections on the transport (M5)

Every arrow marked P2P in the diagram is HTTP carried over `ember-transport` (`FR-N1`, `FR-N5`):
**one transport stream = one HTTP/1.1 connection**, so the server and node routers, their
WebSockets and the client's push channel run unchanged. Nothing outside `crates/transport/` names the
backend; tests use the in-memory `MemNetwork`.

```
 client device ──ember-server/1──▶ ember server ──ember-node/1──▶ ember node
   (Dialer)        gate: devices      (Dialer, one       gate: allowed server
                   table (FR-N3)       connection/node)   peer ids (FR-N3)
                                       │
          ember-exec shim ─http://127.0.0.1:<p>─▶ bridge ─(one stream per TCP conn)─▶ node
          codex app-server ─ws://127.0.0.1:<p>/<secret>─▶ exec-server relay ─▶ node
```

| Piece | Where | Role |
| ----- | ----- | ---- |
| Identity | `transport.key` in the server's data dir / the node's state dir | Persistent Ed25519 key = `PeerId`; printed with the `PeerAddr` at start |
| `PeerGate` | `ember-transport` | Allow-list checked at accept (before any request); revoking closes the peer's open connections |
| `Dialer` | `ember-transport` | One cached connection per (peer, service), a fresh stream per request/WebSocket, re-dial after close |
| `HttpListener::with_gate` | `ember-transport` | `axum::serve` over a gated transport listener |
| `NodeClient` | `crates/node/src/client.rs` | Same API over HTTP (`new`) or the transport (`over_transport`) |
| Devices | `crates/server/src/devices/` | `devices` table → the server's gate; managed on the TCP listener only |
| Computers by peer | `crates/server/src/computers/` | `peer_json` column (migration 5); probes, exec relay and the shim bridge dial through the server's `Dialer` |
| `Api` | `crates/client/src/api.rs` | Same API and push socket over HTTP (`new`) or the transport (`over_transport`) |

Both daemons keep their TCP listener for local use (`ember-term`, the VS Code companion,
loopback admin). Bearer tokens (node API) are kept as a second factor on top of the peer
allow-list for now. How peers find each other beyond address hints (the `darkpyonix.dev`
address directory and device registration under the user's account, `FR-N2`) plugs into
`ember_transport::AddressDirectory` and is not built yet.

---

## 2. The IDE window: what VS Code's own process model dictates

> 09-22 material. "The server" below means **the computer that serves VS Code Web for an IDE
> window**: under the 10-03 model, a project's computer, not the main server.

This is the part of the architecture Ember does not get to design. It is a fact about VS Code,
and Ember's job is to model it accurately, not to wish it were simpler. VS Code's own multi-process
architecture, running server-side under `--serve-web` (or equivalently under `code-server`), is:

| VS Code's own process | Role | Can Ember substitute it? |
| ---------------------- | ---- | -------------------------- |
| **Extension Host** (`LocalProcess` kind: a Node.js child process) | Loads and runs marketplace extension code; exposes the VS Code Extension API (`vscode.*`) to it | **No.** This is `E2`/`D2`, unconditional. See `IMPLEMENTATION.md` §1. |
| **Renderer / Workbench** | In desktop VS Code, an Electron renderer process painting the UI. Under `--serve-web`, this is the JS/CSS bundle that loads *inside the browser/webview*, i.e., inside Ember's editor window's webview. | This is the piece D6/M6 eventually targets for a Compose-native replacement, but not before M6, and even then category-3 (webview-panel) extensions still need *something* webview-shaped. See `IMPLEMENTATION.md` §3–5. |
| **Language servers / debug adapters** | Separate processes already, communicating over stdio with JSON (LSP/DAP) | Already decoupled from VS Code core by design upstream; Ember does not need to do anything special here beyond making sure the server host can spawn them, which `--serve-web` already handles. |
| **Pty Host** | Manages integrated terminal instances | Same as above: already its own process upstream; not a substitution question for Ember. |
| **Shared Process** | Background tasks: storage, telemetry | Not user-visible; not a target for substitution. |
| **Static asset serving** (part of what `--serve-web` bundles together) | Serves the Workbench's HTML/JS/CSS payload to whatever's loading it | **Candidate for substitution.** This is `PROJECT.md` Q1/Q2: whether this can be split from the Extension-Host-owning process cleanly enough for a Rust/Python layer to front it (caching, local serving) without touching anything Extension-Host-related. Tracked in `IMPLEMENTATION.md` §2 as `IMPL-1`. |

The load-bearing distinction, stated plainly: **`--serve-web` bundles "serve the UI" and "run the
Extension Host" close together, but they are not the same responsibility.** The first is a static
(or near-static) file-serving problem any competent HTTP server can do. The second is "run
arbitrary Node.js modules against a large internal API," which nothing but the official
implementation can do without becoming a second, forever-diverging implementation of VS Code
itself. Ember's architecture treats these as separable *in principle* and defers to
`IMPLEMENTATION.md`/Q1 for how separable they turn out to be *in practice* once someone reads the
actual `vs/server` source.

---

## 3. The IDE window bridge, in detail

Per `INTENT.md` D5, the bridge is built on each platform's native webview↔host message-passing
API, not a generic transport:

- **macOS:** `WKScriptMessageHandler`. The injected script calls
  `window.webkit.messageHandlers.<handler>.postMessage(payload)`; the native (Swift/ObjC, called
  from the Rust/Compose host) side registers a handler that receives it as a callback. Native→web
  push (`FR-B4`) goes through `WKWebView.evaluateJavaScript`.
- **Windows:** `WebView2`. `postMessage`/`AddHostObjectToScript`; the latter exposes a COM host
  object whose methods JS can call close to directly, which is the closest available approximation
  to a synchronous call on this platform.
- **Common abstraction:** a Rust trait (`WebviewBridge` or equivalent, naming TBD at
  implementation time) with `send_to_native(payload)` / `send_to_webview(payload)`, implemented
  per-platform underneath, so `FR-B1`–`FR-B4` are specified and tested once against the trait, not
  once per platform.

**What crosses the bridge, concretely, per FR-B2/FR-B4:**

```jsonc
// webview → native, on tab detach (FR-B2)
{
  "kind": "tab_detach",
  "version": 1,
  "sourceWindowId": "...",
  "fileUri": "vscode-remote://...",
  "cursor": { "line": 42, "column": 7 },
  "scroll": { "top": 1180 },
  "selection": { "start": { "line": 40, "column": 0 }, "end": { "line": 42, "column": 7 } }
}

// native → webview, cross-window sync push (FR-B4)
{
  "kind": "sibling_window_closed",
  "version": 1,
  "windowId": "..."
}
```

Implemented as `web/proxy/static/detach.js` (webview) and the `crates/bridge/` crate (native; message
types, `WebviewBridge`, version handling). Concretely: positions are **0-based**; the payload
crosses as a JSON string; `version` is per `kind` (`tab_detach`, `sibling_window_closed`,
`open_window`, all 1); `tab_detach` also carries the additive fields `workspace.folder`,
`screen`, `label`, `editor` and `sentAtMs`; the WKWebView handler is named `emberBridge`, and
native → webview pushes call `window.__emberBridge.receive(json)`.

Schemas are versioned (`"version"`) from the start (per `FR-B2`'s acceptance criteria), because
the launcher and any number of editor windows may be running builds that drifted by a release or
two, and a silent schema mismatch is a worse failure mode than a logged, ignored, versioned one.

**What deliberately does not cross the bridge:** editor content on every keystroke, rendering
state, anything resembling the high-frequency traffic that would actually need JSI-grade
throughput. `NFR-B2`'s 5 ms budget is generous specifically because nothing latency-sensitive is
expected to ride this channel; see `INTENT.md` D5 for why that framing, not raw speed, drove the
transport choice.

---

## 4. Where this document ends and `IMPLEMENTATION.md` begins

This document answers "what runs where and how does it talk." It deliberately does not answer:

- Which categories of VS Code extension survive a Compose-native Renderer and which don't
- What, precisely, breaks if Monaco is removed
- The staged plan from "wrap Monaco" (M1–M5) to "replace Monaco" (M6)

Those are `IMPLEMENTATION.md`'s job, because they are questions about VS Code's *internal* design
(the Extension API surface, the Renderer↔Extension-Host RPC protocol, what each extension category
actually depends on) rather than questions about Ember's process topology.
