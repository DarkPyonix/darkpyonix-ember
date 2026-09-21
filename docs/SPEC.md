# SPEC.md — DarkPyonix Ember

Functional (`FR-*`), non-functional (`NFR-*`), and protocol (`PR-*`) requirements. Every
implementation must trace to an ID here. If a requirement has no test, it is not done — see
`dioxus-compose`'s own SPEC discipline, which this document follows.

Requirements are grouped by area: **L** (Launcher), **B** (Bridge), **W** (Wrapped editor window),
**K** (Kernel/Agent Host integration), and, for the long-term editor core, **M** (Monaco-parity).

---

## §L — Launcher (dioxus-compose, no webview: E1)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | -------------------- |
| **FR-L1** | Display a list of projects. | Renders from local/synced project metadata; no network call required to show cached data on cold start. |
| **FR-L2** | For a selected project, display its assigned server computer(s), with reachability status. | Status (reachable / unreachable / unknown) is polled, not blocking the initial render — a server list renders before reachability is confirmed and updates in place. |
| **FR-L3** | Display a project's agent conversation history, read-only, without requiring an editor window to be open. | Reads conversation state over the project's server connection (see `FR-K3`); renders the same transcript format the in-editor chat surface uses, so the two never visually diverge. |
| **FR-L4** | Clicking a server opens a new native editor window for that server's project (`FR-B3`). | Launcher process itself never creates a webview; it only issues the window-spawn request. |
| **NFR-L1** | Launcher cold start under budget. | ≤ the current `dioxus-compose` NFR-3 distribution/RSS targets (under 100 MB distribution, under 100 MB RSS for an empty window), inherited unchanged, plus Ember's own project/server data model — re-measured at Ember's M1, not assumed to hold automatically. |
| **NFR-L2** | Launcher never allocates a webview under any code path. | A static analysis / lint step in CI that fails the build if any webview-creation API (`WKWebView`, `WebView2`, or the `mcp__Adobe...`-style embedding patterns) appears reachable from launcher-process code. This is `E1` made testable, not just stated. |

---

## §B — Native Bridge (launcher ↔ editor windows, and within an editor window ↔ its webview)

This is the boundary discussed in `INTENT.md` D5: platform webview message-handler APIs, not a
generic socket.

| ID | Requirement | Acceptance criteria |
| -- | ----------- | -------------------- |
| **FR-B1** | The wrapping layer injected into VS Code Web detects a tab drag-out gesture (mousedown on a tab → drag past a threshold distance → mouseup outside the tab strip). | Threshold and capture order do not interfere with VS Code's own internal tab-reorder drag listener — a documented integration test drags a tab a small distance (reorder) and a large distance (detach) and asserts each fires the correct one, not both. |
| **FR-B2** | On detach detection, the webview posts a structured message to the native host containing at minimum: file URI, cursor position, scroll position, selection range, and the originating window's identity. | Message is delivered via the platform's native bridge (`WKScriptMessageHandler` on macOS, `AddHostObjectToScript`/`postMessage` on `WebView2`), not a local socket, per D5. Schema versioned; a version mismatch between injected script and native host is detected and logged, not silently dropped. |
| **FR-B3** | The native host, on receiving a detach (or a launcher-initiated open) request, creates a new native window with an embedded webview pointed at the same project's locally-served VS Code Web instance, passing the initial state from `FR-B2` (or the launcher's initial project selection) so the new window opens directly to the right file/state. | New window appears within one visible frame of user action completing (no spinner-then-window; the window itself may still be loading VS Code Web inside it, per `NFR-B1`). |
| **FR-B4** | The bridge is bidirectional: the native host can push events into a webview (e.g., "your sibling window closed, update your recent-windows list") the same way. | At minimum used for cross-window agent-conversation-sync (`FR-K3`) notifications; extensible without a protocol version bump for additive fields. |
| **NFR-B1** | New editor window open-to-usable time. | ≤ the time a fresh `code-server`/`--serve-web` page load takes today, plus no more than 100 ms of Ember-added overhead for window creation and initial-state injection (p99, measured on the reference machine class used in `dioxus-compose`'s own bench harness). |
| **NFR-B2** | Bridge message overhead. | A single `FR-B2`/`FR-B4` message, encode-to-native-receipt, ≤ 5 ms p99 on the reference machine. This is generous relative to D5's "human-triggered, low-frequency" framing — it exists as a regression guard, not because sub-millisecond latency is required here. |

---

## §W — Wrapped editor window (VS Code Web, official, in a webview)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | -------------------- |
| **FR-W1** | The editor window serves VS Code Web **locally**, against the project's assigned server, such that only API/data calls cross the network — not the UI asset payload on every load. | Static workbench assets are cached/served from a local layer (see `IMPL-2` in `IMPLEMENTATION.md` for what "local" is and is not allowed to mean); a cold open on a slow network still renders the shell promptly and degrades gracefully (not blank-screen) if the server connection is slow. |
| **FR-W2** | The wrapping layer applies CSS/DOM overrides for: OS-native-feeling titlebar treatment, and a responsive layout suitable for narrower (tablet/mobile-class) viewports. | Achieved entirely through injected CSS/JS layered on the official, unmodified VS Code Web build — no upstream source is patched (`D3`). A CI check diffs the served VS Code Web bundle against the upstream release it was pinned to and fails if it has been locally modified rather than wrapped. |
| **FR-W3** | The editor window's OS chrome (native titlebar) is suppressed where the injected VS Code Web titlebar fully replaces its function, so the two do not visually double up. | Verified per-platform (macOS `titleBarStyle`, equivalent Windows treatment); does not regress standard window controls (close/minimize/maximize) reachability. |
| **NFR-W1** | Marketplace compatibility. | Every acceptance test in this document that references "an extension" is run against the actual, unmodified extension from the official marketplace, not a mock — per `E3`. A regression here is a release blocker, not a known-issue. |

---

## §K — Kernel / Agent Host integration (DarkPyonix)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | -------------------- |
| **FR-K1** | Ember's Agent Host addresses a per-project DarkPyonix kernel instance over the project's server connection, using a stable, versioned contract. | Contract lives in this SPEC (or a linked shared-contract document once `PROJECT.md` Q3 resolves) independent of DarkPyonix's internal implementation; a DarkPyonix-side change that does not change the contract requires no Ember change. |
| **FR-K2** | Agent conversation state is visible identically whether observed from inside an open editor window (VS Code Chat-style surface) or from the launcher's read-only viewer (`FR-L3`). | Both read from the same underlying session/transcript representation; no separate "launcher's copy" that can drift from the live one. |
| **FR-K3** | Conversation updates while an editor window is open are reflected in the launcher's viewer without requiring the launcher to poll aggressively. | Push-based update via `FR-B4` where an editor window is open; falls back to reasonable polling when no editor window for that project is open, since there is then no bridge to push through. |
| **NFR-K1** | Agent Host does not become a second implementation of what DarkPyonix already does. | Any logic that decides *what the agent does* lives in DarkPyonix or the harness it runs; Ember's Agent Host code is limited to session lifecycle, transport, and presentation — a code-review-level check per `INTENT.md` D9, not currently a mechanical one. |

---

## §M — Monaco-parity (long-term, gated behind M6 per `PROJECT.md`)

These are **not** committed for the MVP. Listed here so that, when M6 begins, work starts from an
already-written spec rather than an empty page — and so that "is this in scope yet" always has a
written answer (no, until M6 starts) rather than an ambiguous one.

| ID | Requirement | Acceptance criteria (draft — to be firmed up at M6 start) |
| -- | ----------- | ---------------------------------------------------------- |
| **FR-M1** *(draft)* | A Compose-native text buffer and cursor/selection model, IME-correct for CJK input, reusing `dioxus-compose`'s own IME work rather than re-deriving it. | Inherits `dioxus-compose`'s IME acceptance checklist (its `SPEC.md` §6) as a baseline; Ember does not restate IME correctness criteria independently. |
| **FR-M2** *(draft)* | Diagnostics (`vscode.languages.registerDiagnosticsProvider`-sourced) render correctly positioned in the Compose-native buffer, for an unmodified extension. | An existing, popular linter extension (e.g., ESLint) produces visually correct squiggles/markers with zero extension-side changes. |
| **FR-M3** *(draft)* | CodeLens and Hover providers render correctly positioned overlays, for an unmodified extension. | Same bar as `FR-M2`, against an existing CodeLens- or Hover-using extension. |
| **FR-M4** *(draft)* | Inline completion providers (the Copilot-class category — directly relevant to Ember being an *agentic* IDE) render ghost text and accept/reject correctly. | This is treated as the highest-priority item within §M once M6 starts, per `INTENT.md` D7's category ordering, because it is closest to Ember's actual reason to exist. |
| **FR-M5** *(draft)* | Extension-generated webview panels continue to work, contained to their panel, once §M items above have removed Monaco from the steady state elsewhere in the window. | A category-3 extension (per `IMPLEMENTATION.md`) — e.g. a Jupyter notebook cell — renders inside its panel with no visible difference from running it in upstream VS Code Desktop; the rest of the window around it is Compose-native. |
| **NFR-M1** *(draft)* | Overhead versus the wrapped-webview baseline established at M1–M5, for the operations §M covers. | Not yet defined numerically — to be set from real M1–M5 measurements, per `PROJECT.md`'s explicit refusal to schedule or budget M6 before that data exists. |

§M acceptance criteria are intentionally underspecified relative to §L/§B/§W/§K: writing precise
budgets for work that is explicitly not scheduled would be guessing dressed as rigor. They are
firmed up as the first act of M6, not before.
