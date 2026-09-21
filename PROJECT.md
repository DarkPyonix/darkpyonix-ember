# PROJECT.md — DarkPyonix Ember

> New to this project? [`docs/BACKGROUND.md`](docs/BACKGROUND.md) has the full chronological
> reasoning behind every decision referenced below. This document assumes it.

## Scope

Ember is an agentic IDE. Its job is to make three things sit together comfortably:

1. A **native, always-light launcher** — projects, their assigned servers, agent conversation
   history — built on `dioxus-compose`.
2. A **full, unmodified VS Code editing experience** — Workbench, Monaco, Extension Host, and the
   official marketplace — reachable per project without paying its cost when not in use.
3. An **agent runtime** (the DarkPyonix kernel) that the IDE's chat surface and the launcher's
   conversation viewer both talk to.

This document does not re-derive `dioxus-compose`'s own scope, spec method, or non-negotiables.
It assumes them and adds only what Ember needs on top. Where the two disagree, `dioxus-compose`'s
own documents govern its own code; this document governs Ember's.

## Method

Same discipline as `dioxus-compose`: **spec first, then a failing test, then code.** An SPEC ID
(`FR-*`, `NFR-*`, `PR-*`) exists in `docs/SPEC.md` before behavior lands. A decision that changes
scope goes to `docs/INTENT.md` first, in its own commit, with the alternatives it rejected and
why.

The one addition specific to Ember: because a large share of Ember's design work is about
**which pieces of an existing, huge, actively-developed project (VS Code) Ember does and does not
reimplement**, every such decision is tracked in `docs/IMPLEMENTATION.md` against a specific
upstream VS Code architectural fact (extension host process model, `ExtensionHostKind`, the
Renderer↔Extension-Host RPC boundary), not against Ember's convenience. If upstream's actual
architecture and Ember's assumption about it disagree, upstream's architecture is correct and
`IMPLEMENTATION.md` gets corrected, not worked around silently.

## Milestones

| ID | Milestone | Decides |
| -- | --------- | ------- |
| **M0** | Spec-first foundation: this document, `INTENT.md`, `SPEC.md`, `ARCHITECTURE.md`, `IMPLEMENTATION.md` land, cross-referenced, no code | Whether the tension between E1 and E2 (see `INTENT.md`) has an honest, written resolution before anyone writes a line of Rust or Kotlin |
| **M1** | Launcher MVP: project list, server list, static (non-live) agent history view, on `dioxus-compose` — no editor window yet | Whether the launcher can, by itself, hit `dioxus-compose`'s own NFR-9 (indistinguishable-from-hand-written-Compose) budgets while additionally holding Ember's own project/server data model |
| **M2** | Editor window MVP: click a server → new native window → webview → VS Code Web served locally against that server, API calls only crossing the network, no launcher↔editor bridge yet (user closes and reopens manually) | Whether "webview in its own window, everything else outside it stays native" holds up as a real, usable product, before any bridge complexity is added |
| **M3** | Launcher↔editor bridge: tab-detach-to-new-window (`FR-B1`–`FR-B4`), native titlebar treatment on the editor window, live agent conversation sync between an open editor window and the launcher's viewer | Whether the native webview bridge (`WKScriptMessageHandler`/`AddHostObjectToScript`) is fast and reliable enough for interactive use, and whether tab-detach emulation feels acceptable without true Electron-style same-process window splitting |
| **M4** | DarkPyonix kernel integration: agent sessions run against a real DarkPyonix kernel per project, both inline in the editor window and in the launcher's conversation viewer | Whether Ember's Agent Host contract (`FR-K1`–`FR-K3`) is stable enough that DarkPyonix's own roadmap can evolve without breaking Ember on every release |
| **M5** | Responsive/mobile pass on the VS Code Web wrapping layer (`IMPL-2`): CSS/DOM overrides, touch handling, no upstream fork | Whether "wrap, don't fork" (E3, D3) survives contact with real mobile Monaco touch UX, or whether it forces a fork decision that has to go back through `INTENT.md` |
| **M6** | Compose-native editor core, staged rollout starting with the FR-1/FR-2 category (diagnostics, CodeLens, Hover — see `IMPLEMENTATION.md` §3) | Whether removing Monaco from the steady state is worth its cost once real usage data from M1–M5 exists — **this milestone is explicitly not committed to now**; M0–M5 must ship and prove the hybrid model first |

M6 is the "Monaco replaced by Compose" ambition discussed at length in `IMPLEMENTATION.md`. It is
listed here so it is not forgotten, and explicitly gated behind M1–M5 so it cannot become an excuse
to delay a usable product. See `INTENT.md` D8 for why this ordering is a non-negotiable, not a
preference.

## Open questions

| ID | Question | Status |
| -- | -------- | ------ |
| **Q1** | Can the static-asset half of `--serve-web` be split from the Extension-Host half cleanly enough that a Rust/Python layer can front the former without touching the latter? (`IMPLEMENTATION.md` §2) | Open — needs a source-level read of `vs/server` before M2 |
| **Q2** | What is the actual wire protocol VS Code Web's Renderer speaks to the Extension Host, and how stable is it across VS Code releases? | Open — this is the load-bearing fact for whether M6 is even approachable later; no public spec is known to exist, so this may require reverse-engineering against a pinned VS Code version |
| **Q3** | Does DarkPyonix expect to be addressed as a Jupyter-protocol-compatible kernel (so existing Jupyter-aware extensions "just work" against it), or as something Ember's Agent Host must speak a bespoke protocol to? | Open — blocks `FR-K1`, owned by DarkPyonix's own roadmap, tracked here as a dependency |
| **Q4** | For extension-generated webview panels (`vscode.window.createWebviewPanel`) once M6 begins: does Ember keep a general-purpose embedded webview capability for any panel an extension asks for, or does it only special-case a short list of high-value extensions (Jupyter, Markdown preview)? | Open — deferred to M6 planning; premature to decide before M1–M5 ship |
| **Q5** | Titlebar/tab-detach fidelity (`FR-B1`–`FR-B4`): is "new native window opens with the detached file" an acceptable substitute for VS Code Desktop's true same-process tab split, or does user testing at M3 demand closer parity? | Open — decided empirically at M3, not in advance |

## Rejected alternatives (summary — see `INTENT.md` for the full argument)

- **Fork VS Code Web to add native window/titlebar APIs.** Rejected: couples Ember to a
  continuously-rebased fork of a fast-moving upstream, for a feature (tab detach) that can be
  emulated at the wrapping layer instead. See D4.
- **Reimplement the Extension Host in Rust or Python.** Rejected outright, not just deferred: the
  Extension Host is a Node.js child process running a large, evolving, internal RPC surface that
  marketplace extensions depend on directly. See D2 and E2.
- **Ship Ember's editor as Compose-native from day one, no Monaco at all.** Rejected for MVP:
  this is the M6 ambition, and building it first means shipping nothing usable while chasing
  Monaco's decade of text-shaping, IME, and accessibility maturity — the exact trap
  `dioxus-compose` itself was built to avoid for GUI toolkits generally. See D8.
