# INTENT.md — DarkPyonix Ember

## Motivation

Agentic IDEs in the current generation (Orca, Paseo, and most competitors) are webview stacks:
Electron shell, Chromium engine, a JS heap that stays resident for as long as the project is open
— which, for an agent that works in the background, tends to mean *all day, for every open
project*. That is precisely the weight `dioxus-compose` exists to remove from desktop software.
Ember is the decision to apply that argument to an IDE specifically, and to do it without breaking
the one thing that makes VS Code worth building on top of in the first place: its extension
marketplace.

Two problems, and they pull in opposite directions.

**Problem A — the steady-state surface must not be a browser.** A developer using an agentic IDE
spends a lot of time *not* actively editing: watching an agent work, checking which server a
project is assigned to, skimming a conversation history, picking the next project to open. If that
surface costs a browser engine's memory and startup time just to exist, the product has recreated
exactly the weight it was supposed to avoid — it has just moved the weight from "the editor" to
"the thing you look at before you open the editor."

**Problem B — the market only exists inside the official VS Code Extension Host.** VS Code's
marketplace is not a list of standalone plugins; it is a list of Node.js modules written against a
specific, large, internally-versioned RPC API exposed by a specific process, the Extension Host
(`vscode-docs1`, *Extensibility Principles and Patterns*: "The extension host is a Node.js process
and it exposes the VS Code API to extension writers"). There is no version of "keep the
marketplace working" that does not mean "run the official Extension Host, unmodified." This is not
a preference; it is what the word "marketplace" means here.

Ember's actual contribution is not "no webview" (that slogan alone is unachievable while B holds)
and not "full VS Code" (that alone reproduces A). It is the specific, documented place where the
line between them sits, and the discipline to keep that line where the evidence says it should be
rather than where it would be more flattering to claim.

## Non-negotiables

| ID | Constraint |
| -- | ---------- |
| **E1** | The launcher never contains a webview, under any circumstance, for any feature. |
| **E2** | The VS Code Extension Host is always the official, unmodified Node.js implementation. |
| **E3** | An extension that works unmodified in upstream VS Code must work unmodified in Ember. |
| **E4** | A webview, where one exists, is scoped to the smallest region that needs it. |
| **E5** | A Compose-native editor core, if built, must not diverge from VS Code's own behavior — VS Code is correct by definition where the two disagree. |

These are restated from `README.md` here because every decision below is a resolution of the
tension between E1 and E2, and needs them in view.

## Decisions

### D1 — Split the product into a launcher and per-project editor windows, not one window

**Decision.** Ember is not a single VS Code window with a sidebar bolted on. It is a
`dioxus-compose` launcher plus independent, separately-opened editor windows, one per project
server the user has open — the JetBrains Gateway shape, not the VS Code Desktop shape.

**Why.** This is what makes E1 achievable at all. If the launcher and the editor shared a window
or a process, the launcher would inherit the editor's webview by construction. Splitting them into
genuinely separate OS windows/processes means E1 is a fact about the launcher's process, not a
policy someone has to remember to uphold inside a shared one.

**Rejected alternative.** A single-window IDE with a native sidebar and a webview-based main
editor area, à la how some Electron apps embed native panels. Rejected because the sidebar and the
editor area would still share a process and a window, and "no webview in this half of the window"
is a much weaker, much more fragile guarantee than "no webview in this process."

### D2 — The Extension Host is never reimplemented, in any language

**Decision.** Ember does not write a Rust, Python, or other substitute for the Node.js Extension
Host, at any point on the roadmap, including after M6.

**Why.** The Extension Host is not a stateless request/response server that happens to be written
in Node.js — it is a process that loads and executes the actual extension code, using a large,
internal, evolving RPC protocol that VS Code's core team does not publish as a stable spec
(`readoss.com`, on `ExtensionHostKind.LocalProcess`: "full Node API access"). Reimplementing it
would mean re-deriving that protocol from source on every VS Code upgrade, forever, to run code
Microsoft already runs correctly. This is the single most expensive possible way to buy a small
amount of "no Node.js on the server" purity, and E2/E3 exist specifically to foreclose it before
anyone is tempted.

**Rejected alternative.** "Rust or Python server responds to the same requests instead." Explored
directly in `IMPLEMENTATION.md` §2 as Q1/Q2 and found to work only for the static-asset-serving
sliver of what `--serve-web` does — not for anything the Extension Host itself is responsible for.

### D3 — Wrap VS Code Web, don't fork it, for as long as possible

**Decision.** Where Ember needs to change VS Code Web's behavior (responsive layout, titlebar
treatment, tab-detach signaling), it does so through CSS/DOM injection and a `postMessage`/native
bridge layered on top of the official build, not by maintaining a patched fork of the VS Code Web
source.

**Why.** A fork has to be rebased against upstream forever, and upstream moves fast (VS Code ships
roughly monthly). A wrapping layer that reads the DOM and injects scripts is more fragile to *some*
upstream UI changes, but it fails visibly and locally — a selector stops matching — rather than
requiring a merge conflict to be resolved by hand on every release. It also means Ember never has
to publish or maintain a VS Code build of its own, which would itself be a supply-chain and trust
question for every extension author and user.

**Rejected alternative.** Fork `vscode` and `vscode-web`, add native window/titlebar hooks
directly analogous to the ones Electron gives Desktop VS Code (`BrowserWindow`, drag regions).
Rejected per D4 below — tab detach specifically does not need this.

### D4 — Tab detach is emulated at the bridge layer, not reimplemented from Electron's APIs

**Decision.** VS Code Desktop's drag-a-tab-out-to-a-new-window behavior is an Electron-specific
feature (`BrowserWindow.setBounds`, native window creation) that does not exist in VS Code Web's
codebase at all — it is not merely disabled there, the code path is absent. Ember does not port
it. Instead, a small injected script detects a drag-out gesture in the webview, and hands off to
the native shell (`FR-B1`–`FR-B4`): the native side opens a **new webview-backed window** pointed
at the same locally-served VS Code Web instance, with the detached file's URI and (where available)
cursor/scroll/selection state passed along as initial state.

**Why.** This keeps D3 intact — no VS Code source is patched — while still giving users a
close-enough approximation of the behavior they expect from Desktop. It is honestly weaker than
true Electron-style tab splitting (a new window opens rather than a tab visibly detaching from
existing chrome), and `PROJECT.md` Q5 tracks whether that gap matters in practice once users try
it.

**Rejected alternative.** Accept no tab-detach at all, route it through a right-click "Open in New
Window" menu item instead. Still on the table as a fallback if M3 user testing finds the drag
emulation unreliable, but not the starting design, because "drag a tab out" is a strong enough
existing-VS-Code-user expectation to be worth the emulation attempt first.

### D5 — The native bridge uses per-platform webview message-handler APIs, not a generic IPC layer

**Decision.** Communication between a webview-hosted editor window and the native shell uses each
platform's own webview↔native bridge (`WKScriptMessageHandler` / `postMessage` on macOS,
`AddHostObjectToScript` on Windows `WebView2`) behind a common Rust-side trait, rather than a
generic transport like a local WebSocket.

**Why.** True JSI-style same-process, zero-copy calls are structurally impossible here — a webview
is a separate process from the native shell, full stop, so the JSI comparison itself was a
category error once examined closely. Given that a process boundary is unavoidable, the native
message-handler APIs are the closest available approximation: lower overhead than a socket
round-trip, and they do not require standing up a local server just to talk to a window the OS
already knows about. The events this carries (tab-detach signals, window-spawn requests, agent
conversation sync pings) are low-frequency, human-triggered events, not a high-rate rendering sync
channel — so the *speed* difference between this and a WebSocket would not be perceptible either
way; the choice is really about not introducing a socket-server dependency where none is needed.

**Rejected alternative.** Local WebSocket server in the native shell, webview connects as a
client. Not wrong, just unnecessary given the event frequency involved, and it would mean the
native shell process always has a listening socket even when no bridge traffic is happening.
Revisit if a future feature needs cross-window state sync frequent enough that the platform
message-handler APIs' overhead becomes visible.

### D6 — MVP does not touch Monaco; the Compose-native editor core is a separate, gated, later milestone

**Decision.** M1–M5 (`PROJECT.md`) ship an editor experience that is VS Code Web, wrapped, inside
a webview, inside its own window. No Compose reimplementation of Monaco, CodeLens, Hover, inline
completions, or any other editor-surface behavior happens before M6, and M6 is explicitly not
scheduled — it is gated on real usage data from the hybrid model existing first.

**Why.** The tempting version of "no webview" is "reimplement the editor in Compose and remove
Monaco entirely." Examined in `IMPLEMENTATION.md` §3–4, that turns out to require rebuilding a
meaningful fraction of what Monaco is — text shaping, IME, accessibility, TextMate grammar
tokenization, virtual scrolling over large files, and, critically, the exact overlay-positioning
system that CodeLens/Hover/inline-completion extensions (including Copilot-class agent
suggestions, which are core to what an *agentic* IDE is for) depend on. This is the same trap
`dioxus-compose` was built to route around for GUI toolkits generally — "the ecosystem lacks a
mature enough X, so borrow a mature X instead of building a new immature one" — and building
Ember's own immature Monaco-substitute first would repeat the mistake `dioxus-compose` exists to
avoid, at a much larger scale, for the single feature (agent-suggestion overlays) that most
directly serves Ember's actual purpose.

**Rejected alternative.** Build the Compose-native editor core first, ship the wrapped-webview
version only as a stopgap. Rejected: this makes M1 depend on M6-sized work before anything ships,
and repeats the exact "wait for our own immature renderer instead of using someone else's mature
one" trap D6 exists to name. See `PROJECT.md`'s milestone ordering, which is a direct consequence
of this decision, not an independent scheduling choice.

### D7 — When M6 eventually starts, it starts with overlay/diagnostic-style extensions, not webview-panel extensions

**Decision.** If and when the Compose-native editor core work begins, its first target category
is extensions that only report structured data for the host to render — diagnostics, CodeLens,
Hover, inline completions (`IMPLEMENTATION.md`'s category 1–2) — not extensions that ship their
own HTML via `vscode.window.createWebviewPanel` (category 3).

**Why.** Category 1–2 extensions never touch Monaco's rendering code directly; they report data
through the Extension API and let the Renderer draw it. That means a Compose renderer that
correctly implements the same reporting contract is a drop-in replacement from the extension's
point of view, with no cooperation needed from the extension author. Category 3 extensions
*author their own webview content* — Jupyter's notebook cells, Markdown Preview Enhanced, GitLens'
graph views — and no amount of Compose-side work changes that; those extensions will want a
webview for as long as they exist in their current form, independent of anything Ember does.

**Rejected alternative.** Try to eliminate webview panels too, by building a Compose-native
"webview-panel-compatible" surface extensions could target instead. Rejected: this would mean
asking every extension author who currently ships HTML to instead target a new, Ember-specific
API, which violates E3 (works unmodified) on its face. Category 3 webview panels are treated as a
permanent, narrow exception under E4, not a problem to eventually solve away.

### D8 — Milestone ordering is a non-negotiable, not a scheduling preference

**Decision.** `PROJECT.md`'s M1–M5-before-M6 ordering is stated as fixed in that document, and
this decision explains why it is pinned here rather than left as an ordinary planning call that
could slip.

**Why.** D6's argument only holds if it is actually followed under pressure. The specific failure
mode being guarded against: momentum or excitement about the Compose-native editor core (it is, on
its own technical merits, the more interesting and more differentiating piece of the project)
quietly reordering the roadmap so that M1's ship-something-usable goal keeps getting deferred
"until the real editor is ready." Pinning the order here, as a decision with its own ID, means
reordering it requires amending `INTENT.md` in its own commit with a stated reason — the same bar
any other non-negotiable change clears — rather than happening by drift in `PROJECT.md` alone.

### D9 — DarkPyonix is a service Ember's Agent Host talks to, not a dependency Ember vendors

**Decision.** DarkPyonix's kernel runs as its own process per project server, reachable over that
server's existing connection alongside the Extension Host and the VS Code Web static/dynamic
serving. Ember's Agent Host code depends on a stable contract with DarkPyonix (`FR-K1`–`FR-K3`),
not on DarkPyonix's internals or build process.

**Why.** DarkPyonix has its own roadmap, its own performance targets, and its own reasons to exist
independent of Ember (it replaces Jupyter's kernel protocol generally, not just for agentic use).
Coupling Ember to DarkPyonix's internals would mean every DarkPyonix release risks breaking Ember
and vice versa. A stable, narrow contract is the same discipline `dioxus-compose` applies to its
own Host↔Renderer boundary (`PR-2`, `PR-4` in that project's SPEC): only what crosses the boundary
is load-bearing, and it should be as small and as typed as it can be.

**Rejected alternative.** Vendor DarkPyonix as a library inside Ember's own process. Rejected: it
would tie DarkPyonix's language/runtime choices to Ember's, forfeit the "kernel usable outside
Ember too" value DarkPyonix has on its own, and reintroduce exactly the tight coupling `PROJECT.md`
Q3 is trying to keep open until DarkPyonix's own team resolves it from their side.
