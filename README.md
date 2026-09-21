# darkpyonix-ember
DarkPyonix Ember Console (VSCode Compat)

**An agentic IDE with a native shell and no standing webview — built on Compose Multiplatform,
wrapping VS Code's server and Monaco only where a browser engine is genuinely required.**

*The agent lives in Ember. The kernel underneath it is DarkPyonix, a Jupyter-replacement Python
kernel. Ember is the IDE; DarkPyonix is the runtime it talks to.*

---

## 📑 Contents

- [Why this exists](#-why-this-exists)
- [Where this reasoning came from](#-where-this-reasoning-came-from)
- [Status](#-status)
- [The shape of the thing](#-the-shape-of-the-thing)
- [Architecture, one paragraph](#-architecture-one-paragraph)
- [Relationship to dioxus-compose](#-relationship-to-dioxus-compose)
- [Relationship to DarkPyonix](#-relationship-to-darkpyonix)
- [Non-negotiables](#-non-negotiables)
- [Project layout](#-project-layout)
- [Documentation](#-documentation)
- [License](#-license)

---

## 🎯 Why this exists

[Orca](https://tryorca.dev)-class and [Paseo](https://paseo.dev)-class agentic IDEs are, today,
webview stacks: an Electron shell, a Chromium engine, a JS heap, running all day for every open
project. That weight is the same weight `dioxus-compose` was built to remove from desktop apps in
general — see [dioxus-compose's own INTENT.md](https://github.com/DarkPyonix/dioxus-compose/blob/main/docs/INTENT.md)
for that argument in full. Ember is what happens when that argument is pointed at an IDE
specifically, for a product that also has to keep the market's extensions working.

Two commitments meet here, and neither is negotiable:

1. **The shell that is open all day must be native.** A project launcher, a list of assigned
   servers, an agent conversation history — the surface a developer sits in front of between
   actual edits — must not cost a browser engine's worth of memory just to exist.
2. **The extension marketplace must keep working, unmodified.** Ember does not fork VS Code's
   extension host, does not reimplement the Extension API, and does not ask extension authors to
   publish twice. An extension that works in VS Code today must work in Ember today, with zero
   changes.

These two commitments are in tension — (1) wants no webview anywhere, (2) requires a real,
official VS Code extension host somewhere, and that host is a Node.js process that a great many
extensions were built assuming a Monaco-based renderer sits in front of. Resolving that tension
honestly, rather than picking a slogan and hand-waving the rest, is what the documents in this
repository are for.

### The resolution, briefly

Ember does not try to remove the webview everywhere at once. It removes it from the one place
that is open all the time — the launcher — and narrows it everywhere else:

- The **launcher** (project list, assigned servers, agent conversation viewer) is pure
  `dioxus-compose`: no webview, ever, by construction.
- The **editor** for a given project opens in its own window, running VS Code Web served
  **locally** against that project's assigned server, with only API calls crossing the network.
  Today that window is a webview. The long-term goal — not the MVP — is a Compose-native editor
  core (`docs/IMPLEMENTATION.md`) that removes Monaco from the steady state and keeps a webview
  only as a per-panel fallback for the extensions that generate one.
- **Extension-generated webview panels** (`vscode.window.createWebviewPanel` — Jupyter, Markdown
  preview, GitLens graphs) are never eliminated as a *category*, because they are not Ember's to
  remove: the extension author chose to ship HTML. They are contained to the panel that asked for
  them, not inflated into a standing browser engine for the whole window.

---

## 🧵 Where this reasoning came from

Every claim in this README — the Dioxus/Blitz/Compose background, the extension-taxonomy analysis,
the reasons the launcher/editor-window split looks the way it does — was worked out in a specific
order, each step checked against evidence (source reading, live web search) before the next step
built on it. **[`docs/BACKGROUND.md`](docs/BACKGROUND.md) is the full, chronological record of that
reasoning**, written so that someone with none of this conversation's context can read it once and
be caught up completely — not just told the conclusions, but shown why each one was reached and
what alternatives were set aside along the way. Read it first if anything below seems
under-justified; the other documents assume it.

---

## 🚦 Status

A **design-stage project**. Nothing in this repository builds yet; the documents here are the
spec-first foundation `dioxus-compose`'s own process requires before code lands. See
[`PROJECT.md`](docs/PROJECT.md) for milestones and [`docs/SPEC.md`](docs/SPEC.md) for acceptance
criteria.

| Layer | State |
| ----- | ----- |
| Launcher UI (dioxus-compose) | Design only — depends on `dioxus-compose` reaching its own M1 |
| Editor window, webview-wrapped VS Code Web | Designed, not implemented (`IMPL-1` in `IMPLEMENTATION.md`) |
| Launcher ↔ editor-window bridge (tab detach, native window spawn) | Designed, not implemented (`FR-B1`–`FR-B4`) |
| Compose-native editor core (Monaco replacement) | Long-term goal, explicitly out of MVP scope (`IMPL-3`) |
| DarkPyonix kernel integration | Tracked in DarkPyonix's own repository; Ember treats it as an external agent/runtime backend (`FR-K1`) |

---

## 🧭 The shape of the thing

A developer's session looks like this:

1. Open Ember. This is the **launcher**: a dioxus-compose window, native, no webview, opens in
   under a second, sits comfortably under 100 MB idle. It shows projects, each project's assigned
   server computer(s), and a history of that project's agent conversations.
2. Pick a project's server. A **new native window** opens, hosting a webview pointed at that
   server's locally-served VS Code Web instance (`--serve-web`, proxied through a local port —
   see `docs/ARCHITECTURE.md`). This looks and feels like VS Code Desktop: full workbench, full
   Monaco, full extension host, full marketplace.
3. Inside that project's DarkPyonix kernel, the agent runs. Its conversation is visible both
   inline (as VS Code Chat-style UI, unmodified) and — for a project-level overview across many
   servers — back in the launcher's own conversation viewer, which reads the same session state
   over the network without needing the editor window open.
4. Close the editor window when done with that project. Its (comparatively) heavy process goes
   away. The launcher, the thing that was open the whole time, never got heavy in the first place.

This is deliberately closer to **JetBrains Gateway** than to VS Code's own single-window model:
a light front door, heavy per-project sessions opened on demand, nothing shared between them
except a thin control channel.

---

## 🏗 Architecture, one paragraph

```
┌─────────────────────────┐        native webview bridge        ┌──────────────────────────────┐
│   Ember Launcher          │  (WKScriptMessageHandler /          │   Ember Editor Window          │
│   dioxus-compose            │   AddHostObjectToScript, per-window)│   WKWebView / WebView2          │
│   — projects, servers,      │ ────────────────────────────────► │   + VS Code Web (served locally)│
│     agent history           │ ◄──────────────────────────────── │   over a project's server        │
│   no webview, ever           │   tab-detach events, window spawn  │   full Extension Host, full       │
└─────────────────────────┘        requests                       │   marketplace, unmodified          │
                                                                    └───────────────┬───────────────┘
                                                                                    │ WebSocket (Remote
                                                                                    │ ExtensionHostKind)
                                                                    ┌───────────────▼───────────────┐
                                                                    │   Project's assigned server       │
                                                                    │   Official VS Code server          │
                                                                    │   (Extension Host is non-negotiable│
                                                                    │    — see docs/IMPLEMENTATION.md)   │
                                                                    │   DarkPyonix kernel underneath      │
                                                                    └───────────────────────────────────┘
```

Full detail, including why the server side cannot be a Rust or Python reimplementation and where
a thin static-asset layer *can* be substituted, lives in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
and [`docs/IMPLEMENTATION.md`](docs/IMPLEMENTATION.md).

---

## 🔗 Relationship to dioxus-compose

Ember's launcher is a **consumer** of `dioxus-compose`, not a fork of it. Every non-negotiable in
that project (C1–C5, see its own `docs/INTENT.md`) applies unchanged to the launcher. Ember adds
exactly one thing dioxus-compose's core does not need to know about: **a native window-spawn API**
(`FR-B3`) that lets the launcher ask the OS to open a new webview-backed window and hand it a URL
plus initial state. This is a capability of the *host* shell, not a widget in the Compose tree —
it never appears in `rsx!`, and dioxus-compose's widget schema is not extended to know what a
webview is. See `docs/SPEC.md` §B for the exact boundary.

## 🔗 Relationship to DarkPyonix

DarkPyonix is the Python kernel Ember's agent sessions run against — built to replace Jupyter's
kernel protocol with something the agent harness can drive directly, rather than through a
notebook UI. Ember does not embed DarkPyonix's implementation; it talks to a DarkPyonix kernel
instance over the same per-project server connection that serves VS Code Web, as one more service
alongside the Extension Host (`FR-K1`–`FR-K3` in `docs/SPEC.md`). DarkPyonix's own architecture,
performance targets and roadmap live in its own repository and are out of scope here except where
Ember's Agent Host needs a stable contract with it.

---

## ⚖️ Non-negotiables

| ID | Constraint |
| -- | ---------- |
| **E1** | The launcher never contains a webview, under any circumstance, for any feature. If a launcher feature seems to need one, the feature is wrong, not this rule. |
| **E2** | The VS Code Extension Host is always the official, unmodified Node.js implementation. Ember never reimplements the Extension API surface to run marketplace extensions against a substitute host. |
| **E3** | An extension that works unmodified in upstream VS Code must work unmodified in Ember. No extension author is asked to special-case Ember. |
| **E4** | A webview, when one exists at all, is scoped to the smallest region that needs it — a single extension-requested panel, never an entire window's steady-state rendering — once `IMPLEMENTATION.md`'s editor-core work lands. Until then, the editor window as a whole is the acknowledged, temporary exception, tracked explicitly rather than treated as done. |
| **E5** | Ember's Compose-native editor core (`IMPLEMENTATION.md`), if and when built, must not become a second, divergent implementation of VS Code's Extension API. Where its behavior and VS Code's disagree, VS Code's is correct by definition, because E3 outranks any elegance argument on Ember's side. |

---

## 🗂 Project layout

```
ember/
├─ README.md                  # this file
├─ PROJECT.md                 # scope, method, milestones M0–M6, open questions
├─ docs/
│  ├─ BACKGROUND.md            # full chronological reasoning — read this first if new
│  ├─ INTENT.md                # motivation, decisions D1–D9, rejected alternatives
│  ├─ SPEC.md                  # FR-*, NFR-*, PR-* with acceptance criteria
│  ├─ ARCHITECTURE.md          # process/window topology, protocols, the bridge
│  └─ IMPLEMENTATION.md        # the VS Code reimplementation question in full detail:
│                               #   what must stay official, what can be replaced, and the
│                               #   staged plan from "wrap Monaco" to "replace Monaco"
└─ (no code yet — see PROJECT.md M0)
```

---

## 📚 Documentation

| Document | What is in it |
| -------- | -------------- |
| [docs/BACKGROUND.md](docs/BACKGROUND.md) | **Start here if you weren't part of the original discussion.** Full chronological reasoning: why native at all, the Dioxus/Blitz/Compose investigation, how the IDE design was arrived at, step by step, with the evidence checked at each step |
| [PROJECT.md](PROJECT.md) | Scope, method, milestones M0–M6, open questions |
| [docs/INTENT.md](docs/INTENT.md) | Motivation, non-negotiables E1–E5, decisions D1–D9, rejected alternatives |
| [docs/SPEC.md](docs/SPEC.md) | Functional and non-functional requirements, the launcher↔editor-window protocol, acceptance criteria |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Process and window topology, the native webview bridge, VS Code's own process model as it constrains Ember |
| [docs/IMPLEMENTATION.md](docs/IMPLEMENTATION.md) | The full "how much of VS Code can Ember not reimplement" analysis: extension categories, what breaks without Monaco, the staged de-webview-ing plan |

The planning documents (`PROJECT.md`, `INTENT.md`, `SPEC.md`) follow `dioxus-compose`'s
spec-first convention: an SPEC ID before an implementation, always.

---

## 📄 License

Apache License 2.0, matching `dioxus-compose`.
