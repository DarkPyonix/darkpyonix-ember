# IMPLEMENTATION.md — DarkPyonix Ember

## How much of VS Code can Ember not reimplement — and what happens if it does anyway

This document is the detailed answer to the question that sits underneath most of Ember's design:
**"we don't want a standing webview, but we do want the marketplace to keep working — exactly
where does that leave us?"** It is organized as a staged analysis, moving from "what is
unconditionally off the table" to "what is a genuine, gated, long-term option," because the answer
is different at each layer of VS Code's architecture.

Everything here traces back to `INTENT.md` D2, D6, D7, and to `SPEC.md` §M. This document is the
evidence and reasoning those decisions rest on, written out in full rather than summarized.

---

## §1. What is unconditionally official: the Extension Host

VS Code's own documentation is direct about why extensions run in a separate process at all:
misbehaving extension code must not be able to affect the core editor's stability or startup time,
so "VS Code loads and runs extensions in a separate process, the extension host process... The
extension host is a Node.js process and it exposes the VS Code API to extension writers"
(*Extensibility Principles and Patterns*, vscode-docs1).

Marketplace extensions are, without exception, Node.js modules written against that exposed API
(`vscode.window.*`, `vscode.languages.*`, `vscode.workspace.*`, and so on). That API is not a thin
shim over something simpler underneath — it is a "massive, typed RPC protocol" (readoss.com's
description of the Extension Host architecture) connecting the Extension Host process to the
Renderer, marshaled through proxy objects on each side. It is also, notably, not fully public as a
stable spec: it is documented incrementally and evolves with VS Code releases.

**Conclusion, unconditional:** the Extension Host must be the official Node.js implementation,
always, everywhere in Ember's roadmap, including past M6. This is `E2`/`D2`. There is no phase of
this project where "confirm marketplace extensions run against a substitute host" is a goal,
because there is no substitute that would not itself become a second, forever-diverging
implementation of VS Code's most actively-developed internal surface.

One structural nuance worth carrying forward, because it explains why §2 is even askable: VS
Code's own `ExtensionHostKind` enum recognizes three environments an extension host can run in —
`LocalProcess` (a Node.js child process, the desktop default, full Node API access),
`LocalWebWorker` (browser-only APIs, the only option on plain vscode.dev), and `Remote` (a Node.js
process on a separate machine, reached over the network — exactly the SSH/container/WSL remote
development pattern). Ember's topology (`ARCHITECTURE.md` §1) puts the editor window's Renderer on
one machine and the project's Extension Host on another — the server — which is architecturally
identical to VS Code's own `Remote` case, communicating over WebSocket. Ember is not inventing a
new relationship between Renderer and Extension Host; it is reusing one VS Code already ships and
supports, which is a meaningfully lower-risk position than it would be if Ember's topology had no
upstream precedent at all.

---

## §2. What might not need to be official: static asset serving

`--serve-web` (and `code-server`, which follows the same shape) bundles two responsibilities that
are conceptually separable even though they currently ship together:

1. **Serving the Workbench's static payload** — the HTML/JS/CSS that becomes the Renderer once
   loaded in a browser or webview.
2. **Running and managing the Extension Host**, plus the WebSocket endpoint the Renderer talks to
   it and to the workspace/filesystem through.

(1) is, in the abstract, an ordinary static-file-serving problem: cacheable, largely
version-invariant per VS Code release, no Node.js-specific behavior required to serve bytes over
HTTP. (2) is exactly the process discussed in §1 and is not substitutable.

**The open question (`PROJECT.md` Q1)** is how cleanly (1) can be split from (2) in practice. Two
sub-questions that gate any real answer:

- Does `--serve-web`'s HTTP layer expose (1) and (2) as genuinely separate routes/ports that a
  reverse proxy could split, or are they interleaved closely enough (e.g., session/auth state
  shared across both) that splitting them risks subtle breakage?
- Even if splittable, is the actual *win* worth the complexity? A Rust/Python static-file proxy in
  front of (1) would reduce *some* load and allow local caching, but the Extension Host process —
  the heavy, memory-relevant piece — is unaffected either way. This is explicitly **not** a path
  to "no Node.js at all"; at best it is "less Node.js doing less work," which is a real but modest
  win.

**Conclusion, provisional:** treat this as a worthwhile optimization to investigate once M1/M2 are
real and there is a concrete deployment to measure, not as a load-bearing part of the architecture.
`ARCHITECTURE.md` §2 marks it as a "candidate for substitution," correctly weaker language than
§1's "unconditionally official."

---

## §3. What breaks, specifically, if Monaco is removed — the extension taxonomy

This is the core analysis behind `INTENT.md` D6 and D7, and behind why `SPEC.md` §M is staged the
way it is. Marketplace extensions interact with the rendered editor surface in three structurally
different ways, and Monaco's removability is a different question for each.

### Category 1 — Pure Extension Host logic, no editor-surface rendering at all

Linters (in the sense of producing a report, not drawing squiggles), formatters, Git integration
logic, debug adapter clients — these compute results inside the Extension Host and either apply a
workspace edit directly or hand a result to the Renderer through an API that describes *what* to
show, not *how* to draw it.

**Monaco dependency: none.** These extensions would function identically against any Renderer that
correctly implements the relevant slice of the Extension API's reporting contract. This category
is not actually a design risk for M6; it is listed for completeness.

### Category 2 — Editor-surface overlays: diagnostics, CodeLens, Hover, inline completions

This is the category `INTENT.md` D7 identifies as M6's correct starting point, and it is worth
being precise about *why* it is both tractable and the highest-value target.

An extension using `registerDiagnosticsProvider`, `registerCodeLensProvider`,
`registerHoverProvider`, or `registerInlineCompletionItemProvider` reports **structured data**:
line/column ranges, severities, markdown content, ghost-text strings, accept/reject commands. It
does not ship pixels. The Renderer — currently Monaco — is responsible for taking that structured
data and drawing it at the correct position in the currently-rendered text, using the Renderer's
own text-layout/glyph-position calculations to do so.

**Monaco dependency: real, but contained to layout math, not extension cooperation.** A
Compose-native Renderer that (a) implements the same reporting contract on the Extension-API side
and (b) does its own correct text-layout-to-screen-position mapping would be a legitimate drop-in
replacement from the extension author's point of view — no changes required on their end, which is
exactly what `E3` demands. The work is real (rebuilding TextMate-grammar-driven tokenization for
syntax highlighting, a correct multi-cursor/selection model, IME composition handling reusing
`dioxus-compose`'s own IME work per `SPEC.md` `FR-M1`, virtual scrolling over large files, and the
overlay-positioning math itself) but it is *bounded* work with a clear correctness target: match
what Monaco already does, extension-visibly.

Inline completions (`FR-M4`) sit in this category and matter more than the others for Ember
specifically: Copilot-class agent-suggestion UI is exactly the "ghost text + ranked
accept/reject" pattern this category covers, and it is the single feature most directly connected
to Ember being an *agentic* IDE rather than a generic one. `INTENT.md` D7 names this the
highest-priority item within category 2 for that reason.

### Category 3 — Extensions that ship their own rendered content: webview panels

`vscode.window.createWebviewPanel` hands an extension author a full HTML/CSS/JS surface to fill
however they like. Markdown Preview Enhanced, Jupyter's notebook cell rendering, GitLens' commit
graph views, REST Client's response viewer — these extensions are not reporting structured data
for a Renderer to draw; **they are shipping a self-contained web app inside a panel**, by design,
because that is what the API is for.

**Monaco dependency: irrelevant — this is not about Monaco at all.** Removing Monaco changes
nothing about category 3 extensions, because their content was never Monaco's to render in the
first place; it was always the extension's own HTML running in its own webview context. The
question these extensions actually pose is not "can Ember avoid Monaco" but "can Ember avoid a
webview *anywhere*," and for this category the honest answer is **no, not without asking every
such extension's author to rewrite their extension against a new, Ember-specific rendering API** —
which `E3` forecloses on its face (see `INTENT.md` D7's rejected alternative).

### Category 4 — Extensions calling into Monaco's own API directly

A smaller set of extensions, typically older or needing low-level editor control, call
`monaco.editor.*` APIs directly from within a webview context rather than going through the
Extension Host's reporting APIs. These extensions have a hard dependency on Monaco *as an
implementation*, not merely on "some Renderer that draws the same things Monaco draws." No
Compose-native Renderer, however correct its category-2 behavior, satisfies this category by
construction, because these extensions are calling a specific library's API surface, not a
protocol.

**Conclusion:** this category is the sharpest edge of "Monaco removal breaks things," and no
amount of category-2-style protocol-matching fixes it. It is not currently sized (how many
marketplace extensions actually fall here is an open empirical question, not yet answered by
anything in this document) — flagged as a gap, see §6.

---

## §4. Summary table

| Category | Example extensions | Monaco-removal impact | M6 treatment |
| -------- | ------------------- | ----------------------- | -------------- |
| 1 — Extension Host logic only | Most linters, Git integration, debug adapters | None | Not a design target; works regardless |
| 2 — Structured overlay data | Diagnostics, CodeLens, Hover, inline completions (Copilot-class) | Real but bounded — a correct protocol-compatible Renderer suffices | **First target**, per `INTENT.md` D7 and `SPEC.md` `FR-M2`–`FR-M4` |
| 3 — Self-shipped webview content | Jupyter, Markdown Preview Enhanced, GitLens graphs | Not actually about Monaco; about webviews generally | **Permanently excepted** under `E4`, contained per-panel — `SPEC.md` `FR-M5` |
| 4 — Direct `monaco.editor.*` API calls | A smaller, unsized set of low-level extensions | Total — no protocol-level fix exists | **Unresolved**, see §6 open gap |

---

## §5. The staged de-webview-ing plan

This restates `PROJECT.md`'s milestones through the lens of "how much webview is present," because
that framing makes the actual trajectory clearer than the milestone IDs alone do:

| Stage | What's a webview | What isn't |
| ----- | ------------------ | ------------ |
| **M1** (launcher only) | Nothing yet built that has one | The entire launcher, always |
| **M2–M5** (wrapped VS Code Web) | The entire editor window's content area — Workbench, Monaco, everything | The launcher (still, always, per `E1`) |
| **M6, once started, categories 1–2 land** | Category-3 extension panels only | The launcher; the rest of the editor window's steady-state rendering (buffer, diagnostics, CodeLens, Hover, inline completions) |
| **M6, category-3 handling matures** (`FR-M5`) | A single panel, exactly when and only when a category-3 extension is active and visible | Everything else, all the time |

The end state this plan converges toward, if M6 is pursued to completion, is **not** "zero
webviews ever" — `INTENT.md` correctly does not claim that as achievable, because category 3 makes
it structurally impossible without violating `E3`. The end state is **"a webview never exists
except while a specific extension that authored its own HTML is actively displaying it,"** which
is the substance behind `E4`'s "smallest region that needs it" language. That is a materially
different, and materially more honest, claim than "no webview," and it is the one this project
actually stands behind.

---

## §6. Open gaps in this analysis

Named explicitly rather than left implicit, per the general house style of surfacing what is not
yet known:

- **Category 4 is not sized.** No data yet on what fraction of marketplace usage this represents.
  Until it is, `NFR-M1`-style budgeting for M6 cannot account for "extensions that will simply
  never work without Monaco specifically," and the honest user-facing framing for M6 (if it ships)
  needs to say plainly that a small, currently-unknown set of extensions may not function.
- **The Renderer↔Extension-Host RPC protocol's stability across VS Code versions is unmeasured.**
  `PROJECT.md` Q2 flags this as the single fact that most determines whether category-2 work done
  at one VS Code version survives the next one without rework. No public spec is known to exist;
  answering this requires either finding an unofficial one or reverse-engineering against a pinned
  version, and either way the answer bounds how realistic M6 is at all, not just how it's paced.
- **§2's split-the-static-serving question is unverified against actual `--serve-web` source.**
  Everything in §2 is reasoning from the *shape* of the problem, not from having read the code
  yet. This is explicitly called out in `PROJECT.md` Q1 as needing a source-level pass before M2.
