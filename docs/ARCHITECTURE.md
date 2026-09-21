# ARCHITECTURE.md — DarkPyonix Ember

This document describes process and window topology: what runs where, what talks to what, and —
critically — which of VS Code's own architectural facts constrain Ember's choices rather than the
other way around. `IMPLEMENTATION.md` covers the deeper question of *why* those facts are what
they are and what can and cannot be substituted; this document is the map, not the argument.

---

## 1. Topology, end to end

```
┌───────────────────────────┐
│      Ember Launcher          │   dioxus-compose · native · no webview (E1)
│      (one process, always      │
│       running while Ember       │
│       is open)                   │
└──────────────┬────────────┘
               │ FR-B3: window-spawn request
               │ (project + server selection)
               ▼
┌───────────────────────────┐        WebSocket / HTTPS         ┌────────────────────────────┐
│   Ember Editor Window #N      │ ───────────────────────────► │  Project N's assigned server   │
│   (one process per open         │   API + Extension Host RPC    │                                  │
│    project; opened/closed         │   traffic only — never the      │  ┌──────────────────────┐   │
│    independently of the             │   full UI asset payload on       │  │ VS Code server (official)│   │
│    launcher and of each                │   every load, per FR-W1            │  │  — Extension Host          │   │
│    other)                                │                                        │  │    (Node.js, E2)              │   │
│                                                │                                        │  │  — Language servers,          │   │
│  ┌─────────────────────┐                     │                                        │  │    debug adapters              │   │
│  │  WKWebView / WebView2    │                     │                                        │  │  — static asset server         │   │
│  │  loading VS Code Web         │ ◄───────────────────┘                                        │  │    (candidate for a thin        │   │
│  │  (official, wrapped              │                                                                │  │    Rust/Python front —          │   │
│  │   per FR-W2/FR-W3, not               │                                                                │  │    see §2 and IMPL-1)             │   │
│  │   forked, per D3)                       │                                                                │  └──────────────────────┘   │
│  └──────────┬──────────┘                                                                                                                    │
│              │ FR-B1/B2/B4: platform                                                                    ┌──────────────────────┐   │
│              │ webview message-handler                                                                     │ DarkPyonix kernel           │   │
│              │ bridge (D5) — tab detach,                                                                    │  (per project, FR-K1)          │   │
│              │ window-state events                                                                        └──────────────────────┘   │
│              ▼                                                                                                                              │
│  ┌─────────────────────┐                                                                                                                    │
│  │  Native shell process     │                                                                                                                    │
│  │  (owns the webview,           │                                                                                                                    │
│  │   handles FR-B1–B4)              │                                                                                                                    │
│  └─────────────────────┘                                                                                                                    │
└───────────────────────────┘                                                                            └────────────────────────────┘
```

Two things to notice, because they are easy to get backwards:

- **The launcher never talks to a project's server directly for editing.** It talks to it only
  for `FR-L2` (reachability), `FR-L3` (read-only conversation history), and `FR-B3` (asking the
  native shell to open a window). It has no code path that renders editor content, because it has
  no code path that *could* — there is no webview in that process (`NFR-L2`).
- **Each editor window is its own process, independent of the launcher's process and of every
  other editor window's process.** Closing one does not affect another; closing the launcher does
  not close open editor windows (though it may, depending on `PROJECT.md` Q5's eventual answer,
  prompt about orphaned windows — undecided).

---

## 2. Inside "the server": what VS Code's own process model dictates

This is the part of the architecture Ember does not get to design — it is a fact about VS Code,
and Ember's job is to model it accurately, not to wish it were simpler. VS Code's own multi-process
architecture, running server-side under `--serve-web` (or equivalently under `code-server`), is:

| VS Code's own process | Role | Can Ember substitute it? |
| ---------------------- | ---- | -------------------------- |
| **Extension Host** (`LocalProcess` kind: a Node.js child process) | Loads and runs marketplace extension code; exposes the VS Code Extension API (`vscode.*`) to it | **No.** This is `E2`/`D2`, unconditional. See `IMPLEMENTATION.md` §1. |
| **Renderer / Workbench** | In desktop VS Code, an Electron renderer process painting the UI. Under `--serve-web`, this is the JS/CSS bundle that loads *inside the browser/webview*, i.e., inside Ember's editor window's webview. | This is the piece D6/M6 eventually targets for a Compose-native replacement — but not before M6, and even then category-3 (webview-panel) extensions still need *something* webview-shaped. See `IMPLEMENTATION.md` §3–5. |
| **Language servers / debug adapters** | Separate processes already, communicating over stdio with JSON (LSP/DAP) | Already decoupled from VS Code core by design upstream; Ember does not need to do anything special here beyond making sure the server host can spawn them, which `--serve-web` already handles. |
| **Pty Host** | Manages integrated terminal instances | Same as above — already its own process upstream; not a substitution question for Ember. |
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

## 3. The bridge, in detail

Per `INTENT.md` D5, the bridge is built on each platform's native webview↔host message-passing
API, not a generic transport:

- **macOS:** `WKScriptMessageHandler`. The injected script calls
  `window.webkit.messageHandlers.<handler>.postMessage(payload)`; the native (Swift/ObjC, called
  from the Rust/Compose host) side registers a handler that receives it as a callback. Native→web
  push (`FR-B4`) goes through `WKWebView.evaluateJavaScript`.
- **Windows:** `WebView2`. `postMessage`/`AddHostObjectToScript` — the latter exposes a COM host
  object whose methods JS can call close to directly, which is the closest available approximation
  to a synchronous call on this platform.
- **Common abstraction:** a Rust trait (`WebviewBridge` or equivalent — naming TBD at
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

Schemas are versioned (`"version"`) from the start — per `FR-B2`'s acceptance criteria — because
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
