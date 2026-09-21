# BACKGROUND.md — How Ember's design was arrived at

This document exists for one purpose: so that someone (or some agent) who was **not** present for
the reasoning that produced `INTENT.md`'s decisions can read this once and have the same context
as someone who was. It is a chronological record of the argument, not a spec — no `FR-*`/`NFR-*`
IDs live here, and nothing here is itself a requirement. Where a piece of reasoning below produced
a requirement or a decision, it is cross-referenced to the ID in `INTENT.md` or `SPEC.md` that
came out of it.

Read this before `INTENT.md` if the *reasoning* is unclear; read `INTENT.md` first if only the
*conclusions* are needed.

---

## Part 1 — Why native at all: the Dioxus/Blitz/Compose background

Ember's launcher is built on `dioxus-compose`, a separate project. Understanding *why*
`dioxus-compose` exists, and why it made the specific bet it made, is necessary background for
understanding why Ember inherits that bet rather than re-litigating it.

### 1.1 The starting complaint: web-stack desktop apps are heavy

The concrete frustration that started this whole line of reasoning: existing agentic-IDE products
(named examples: **Orca**, **Paseo**) are built on web technology stacks end to end — Electron or
similar, a full browser engine, a JS runtime — and the *quality* of that stack, not just its
weight, was the complaint. Sluggishness, memory footprint, a general sense that it doesn't feel
like a "real" native app. This is the origin of the decision to go native instead of accepting the
industry-standard webview stack.

### 1.2 Dioxus, as a framework, and its rendering options

Dioxus is a Rust UI framework, React-like in its component/hooks model, using `rsx!` macro syntax
resembling HTML/JSX. It targets web, desktop, and (with varying maturity) mobile. For non-web
targets it has historically had two rendering strategies:

- **Webview-backed** (the older, more mature path): render Dioxus's virtual DOM output into an
  embedded system webview (comparable to what Tauri does) — this still pays the "browser engine
  per app" cost the whole exercise was trying to avoid.
- **Blitz**: Dioxus Labs' own from-scratch native renderer, aiming to draw Dioxus's HTML/CSS
  output directly via GPU (WGPU), with no embedded browser at all.

### 1.3 A slide claimed Dioxus has "fully native performance" on iOS/Android/desktop — is that true?

The investigation started from a marketing slide (image shared in conversation) claiming, among
other performance figures, that Dioxus offers "fully native performance" on Web, Backend, Desktop,
iOS, and Android, alongside a js-framework-benchmark-style table (vanillajs, solid.js, svelte,
dioxus, react-hooks, react-redux-hooks compared on row-creation/update/removal benchmarks in a
browser).

**Finding:** the claim is half true, half marketing overreach.

- True in the sense that Dioxus *can* use native renderers (not always a webview) on
  desktop/mobile, and Rust itself has no GC overhead, so raw logic execution genuinely can match
  native code.
- Overreach in that: (a) the benchmark table shown is a **web/DOM benchmark**
  (js-framework-benchmark style, browser-only), not a measurement of the iOS/Android native build
  at all — so it does not actually support the "fully native" claim for those platforms; (b)
  "native widgets available" and "always performs identically to native" are different claims —
  actual performance depends heavily on which rendering backend is used and how optimized the app
  is.

### 1.4 What Blitz actually is: a from-scratch HTML/CSS rendering engine

Blitz, specifically, is DioxusLabs' own renderer: it uses **WGPU** (via **Vello**, a Rust
GPU-based 2D renderer that plays the role Skia plays for Compose/Flutter/Chrome/Android) to draw.
CSS parsing uses **lightningcss** (formerly ParcelCSS); layout uses **Taffy**, a Rust flexbox/grid
layout engine. Later versions moved to **Stylo**, Firefox's own parallel, browser-grade CSS
resolution engine, reusing a genuinely mature component rather than reinventing CSS cascade
resolution from scratch — a meaningful signal of increasing seriousness.

The appeal, laid out at the time: this is structurally the opposite of Electron/Tauri-style
webview embedding — instead of shipping a full browser engine per app, Blitz aims to ship only the
narrow slice (HTML parse → CSS layout → paint) needed to render UI, as a lightweight Rust library
linked directly into the app process. This is where figures like "under 3 MB desktop/mobile
footprint" and "under 100 ms startup" (also seen on the original marketing slide) come from
structurally, if achieved.

The three-layer stack, for precision, since this distinction came up explicitly and matters for
later reasoning (§1.9 onward):

1. **HTML/CSS** — the markup/style layer, what the developer authors
2. **Layout engine** (Taffy) — computes box positions/sizes from the CSS box model
3. **Paint backend** (Vello, playing Skia's role) — turns computed layout into actual pixels

### 1.5 Virtual DOM and its relationship to Blitz

Dioxus itself uses a **Virtual DOM (VDOM)**, React-style: components re-render into a new virtual
tree on state change, that tree is diffed against the previous one, and only the resulting patch is
applied to whatever the actual renderer is (browser DOM for the web target, or Blitz's internal
tree for the native target, which Blitz then re-lays-out and re-paints via Vello).

This is a separate layer from Blitz. Blitz is "how pixels get drawn once Dioxus decides what
changed"; Dioxus's VDOM is "how Dioxus decides what changed" in the first place. They compose:
VDOM diff → mutation patch → applied to Blitz's tree → Blitz re-lays-out/re-paints the affected
region.

### 1.6 Compose's different approach: recomposition, not diffing

Jetpack Compose does **not** use VDOM diffing. Its compiler plugin statically analyzes which
`State` reads occur in which composable functions, so at runtime, when a state value changes,
Compose already knows — without any tree comparison — exactly which composables need to re-run
("recompose"). This is, in principle, a lower-overhead mechanism than VDOM diffing, because it
skips the "compare two trees to find what changed" step entirely; the dependency is known ahead of
time rather than discovered by comparison.

**Follow-up question raised and answered:** does this mean Dioxus (VDOM-based) can never benefit
from anything like recomposition's efficiency? Answer: Dioxus is not a purely naive full-VDOM-diff
system either. Its more recent versions lean on **Signals**, a fine-grained-reactivity pattern
closer to Solid.js's than to classic React `useState` — a `Signal` tracks exactly which UI elements
read it, so updating a Signal can patch just those elements without re-running and re-diffing an
entire component subtree. (This is also *why* solid.js and svelte — both signal-based frameworks —
appear as comparison points in the original benchmark slide; it's not incidental.) So the accurate
characterization is: Dioxus retains a React-shaped "component function re-executes, produces a
tree, diff against previous" structure at its core, but Signals let well-written Dioxus code avoid
much of that overhead on specific update paths, without being a strict compile-time-tracked
recomposition system the way Compose is.

### 1.7 Does Dioxus/Blitz have a real shot at beating Compose on performance?

Investigated directly, with the honest conclusion: **unproven either way, and the honest framing
depends on which axis you're asking about.**

**Where Dioxus/Blitz plausibly wins:**
- Startup time and idle memory: a native Rust binary with a lightweight runtime has no JVM to
  start or keep resident, versus Compose's JVM-based runtime overhead (class loading, JIT warmup).
- No GC: Rust has none; the JVM's GC can cause pause-driven frame drops under memory pressure that
  Rust structurally cannot exhibit.

**Where Compose plausibly wins, structurally, independent of raw language speed:**
- Diff-free recomposition (§1.6) — an algorithmic advantage Dioxus's Signal-based mitigation
  narrows but does not eliminate; some diff/patch-generation cost remains on some update paths.
- Skia's decade-plus of production hardening (Android, Chrome, Flutter all use it) versus Vello's
  comparative youth — GPU driver compatibility, batching, caching strategies are all areas where
  raw engine maturity matters independent of the language or architecture choice.
- Compose targets native platform layout conventions directly; Blitz recomputes CSS flexbox/grid
  layout every update, which is a more general-purpose (and typically heavier) computation than a
  platform-specific layout system tuned for its one target.

**Conclusion actually reached:** no credible independent benchmark exists comparing Dioxus/Blitz
native builds against Compose head-to-head (confirmed by direct web search — see §1.8). The
honest position is "plausible edge in specific scenarios (lightweight apps, fast startup, GC-averse
workloads), structural handicap in others (complex UI, heavy animation, frequent updates) that
would require significant further engineering to close," not a claim that either side wins
generally.

### 1.8 Blitz's actual status, verified by direct search

A live web search was performed specifically to check Blitz's real-world performance/maturity
claims (query: "Blitz renderer Dioxus performance benchmark 2026"). Findings, from Dioxus Labs' own
repositories and forks:

- **No published, credible performance benchmark exists for Blitz** as of the check. This is a
  stated absence, not a "benchmarks show X" finding.
- Blitz's own repositories consistently self-describe as **pre-alpha / experimental / WIP**: "many
  CSS properties aren't supported," "many types of events aren't handled," "no support for
  images/videos or multimedia," and explicit statements like "we would not yet recommend building
  apps with it."
- A GitHub Discussion internal to Dioxus Labs ("The Future of Blitz," DioxusLabs/dioxus#1519) shows
  the *core team itself* debating whether Blitz is worth continuing to build out versus adopting
  Servo's components instead, versus discontinuing it — i.e., Blitz's own direction is
  acknowledged as unsettled by its own maintainers, not just by outside observers.
- Blitz has moved to using **Stylo** (Firefox's CSS engine) for CSS resolution, a genuine
  maturity-improving signal (reusing a proven component rather than reinventing cascade
  resolution).

### 1.9 The crucial gap: Blitz has no JavaScript engine

This is the pivot point of the entire background discussion, arrived at through direct
cross-examination rather than assumed:

The reasoning chain that led here: "if Blitz's whole goal is rendering *web* content in Rust, and
essentially no real-world web content is JS-free, doesn't Blitz's goal become incoherent the moment
JS is required — at which point wouldn't a real webview just be faster to reach for?"

Checked directly via search (query: "Blitz dioxus JavaScript engine support blitz-script").
**Finding: confirmed correct.** The official DioxusLabs/blitz repository states outright: "We
don't yet have Blitz bindings for other languages (JavaScript, Python, etc) but would accept
contributions along those lines." A third-party fork (`pathscale/ps-blitz`) is in the process of
adding one, carrying "the JavaScript engine from upstream's unmerged draft PR #491
(`blitz-script`)" — i.e., even where JS support exists at all, it is not in Blitz core; it is an
unmerged draft carried by a downstream fork.

**The resolution this produced:** Blitz's actual, accurate positioning is **not** "a way to
re-render existing JS-driven web content natively." The `blitz` wrapper crate itself is described,
by its own maintainers, as useful for "previewing HTML and/or markdown files" but "currently lacks
interactivity" — i.e., it's closer to a document viewer than a browser replacement. The
interactive path, `dioxus-native`, gets its interactivity **from Dioxus's own event handling (Rust
code)**, not from executing arbitrary JS.

So Blitz's real scope is: **"use HTML/CSS as a markup vocabulary for a UI whose logic is written
from scratch in Rust (via Dioxus)," not "run existing web applications, including their JS, without
a browser."** Framing Blitz as "the web, reimplemented in Rust" overstates its actual ambition;
"a Rust-native framework that happens to use HTML/CSS syntax and needs no JS because the logic was
never JS to begin with" is the accurate framing. This reframing directly informs why Ember does not
expect Blitz (or anything like it) to be a route to running VS Code Web's actual content — VS Code
Web is exactly the kind of pre-existing, JS-heavy application this framing excludes.

### 1.10 Is "you can write UI in HTML/CSS syntax" actually a strong selling point?

A further challenge raised and explored: if you can't bring over React or its ecosystem (because
there's no JS runtime), how much is "familiar HTML/CSS syntax" really worth? React/Vue's actual
strength isn't the markup syntax — it's hundreds of thousands of npm packages, a large developer
pool, mature devtools, and styling ecosystems (Tailwind, shadcn, etc.). Dioxus/Blitz brings across
none of that; only the syntax resembles something familiar, and even that requires writing all
logic fresh in Rust.

**Conclusion reached:** the "you already know HTML/CSS" pitch is real but thin — its actual
surviving value is mostly "flexbox/grid is an already-understood layout model" and "styling can be
kept separate from logic," not much beyond that. This directly informs the assessment of
`dioxus-compose` (§1.11) as the more honest design: rather than pretend to offer "web familiarity"
it can't really deliver on, it drops the HTML/CSS pretense and offers Compose's actual UI paradigm
(Column/Row/Box, not div/span) through Rust syntax — a narrower but more truthful value
proposition.

### 1.11 `dioxus-compose`: the project this background eventually connects to

Partway through this line of inquiry, a specific project was introduced directly: **the user's own
repository, `DarkPyonix/dioxus-compose`** (fetched and read in full during the conversation). Its
actual design, in brief (full detail lives in that repository's own `README.md`/`INTENT.md`/
`SPEC.md`, not restated exhaustively here):

- **Core idea:** author UI in Rust using `rsx!`, hooks, and Signals (Dioxus's `dioxus-core`
  VirtualDom); mutations produced by that VDOM cross a narrow C ABI boundary; a Kotlin/Compose
  interpreter on the other side materializes them as a **real Compose tree** — Compose's own text
  layout, widgets, and platform IME, not a Rust-native reimplementation of any of those.
- **Why:** two stated problems. (1) Web-stack desktop apps (the Electron/Tauri-class problem from
  §1.1) are heavy for an always-open app — memory and download size, not responsiveness, is the
  actual cost. (2) The Rust GUI ecosystem lacks Compose-grade text handling — specifically
  **IME**: Korean/Japanese/Chinese input composition is explicitly named as a non-negotiable
  quality bar, not a nice-to-have, given the maintainer's own context. This maps directly onto
  §1.8's finding that Blitz is not there yet on CSS coverage, event handling, or multimedia, let
  alone IME.
- **How it avoids the JVM cost while keeping Compose's rendering quality:** the Kotlin side is
  compiled ahead-of-time into a native shared library (**GraalVM native-image** on desktop,
  **Kotlin/Native** on iOS), so no JVM ships in the final artifact — Compose's renderer without
  Compose's usual runtime cost.
- **Non-negotiables (that project's own, restated here because Ember explicitly inherits them):**
  no webview (C1), no bundled JVM in shipped artifacts (C2), no hand-written JNI/cinterop glue —
  boundary shims are generated (C3), UI is authored declaratively in Rust, Kotlin is a renderer
  implementation detail (C4), Compose-grade text/IME/widget quality never bypassed (C5).
- **Status at time of reading:** young, spec-first, not on crates.io, API expected to change.
  macOS (arm64) works end-to-end, including basic Korean IME input; Windows/Linux native builds are
  designed but not scripted; iOS/Android/Web are designed, not implemented. Weight, self-reported
  as approximate (not yet independently benchmarked): roughly 64 MB renderer + 21 MB Skia, versus
  roughly 80–120 MB for Compose with a bundled JVM, versus far heavier for a webview stack — with
  an explicit under-100-MB distribution/RSS target still in `Draft` status pending measurement.
- **Measured (Host/Rust side only, as of the read):** click→dispatch→diff→encode at 13.3 µs p99
  against a 500 µs budget; encoding 100 mutations at 2.9 µs p99 with zero steady-state
  allocations; streaming 100 appends into a 10,000-message conversation at 220 µs p99. Explicitly
  **not yet measured**: Renderer-side (Compose/Kotlin) frame timing, and the project's own
  stated goal of ≤10% frame-time overhead versus a hand-written Compose baseline.
- **Why AOT-compiling Compose Desktop on macOS specifically required extra work:** Compose
  Desktop's window is backed by AWT (a `JFrame`), and upstream GraalVM skips AWT support entirely
  on Darwin (a known, still-open upstream GraalVM issue), so the project depends on **BellSoft
  Liberica NIK 25 Full** (a GraalVM distribution that links AWT statically) plus three small C
  shims to satisfy runtime-loaded-by-path dependencies (`libawt_lwawt.dylib`, `libjawt.dylib`,
  `JNI_OnLoad_osxui`) that a statically-linked macOS AWT build otherwise expects to find and
  doesn't. This detail matters for Ember only insofar as it illustrates how deep the
  dependency chain under `dioxus-compose` actually runs — see §1.14.

### 1.12 Does this design make sense given §1.9's finding that Dioxus itself doesn't need a webview?

A direct challenge was raised here: if Dioxus's own native path (Blitz) doesn't use a webview
either, does `dioxus-compose`'s existence become pointless? And separately: doesn't Dioxus's VDOM
diffing mean recomposition-style efficiency is unreachable regardless of which renderer sits
underneath?

**Resolution reached on the first point:** no — `dioxus-compose` occupies a genuinely third
position, distinct from both of Dioxus's existing options:

- Not the webview path (heavy, exactly what both projects are trying to avoid)
- Not Blitz (still pre-alpha per §1.8, missing exactly the maturity — text shaping, IME,
  accessibility — that `dioxus-compose`'s own stated motivation names as the gap it's filling)
- Instead: **delegate rendering entirely to Compose**, a renderer with a decade of production
  hardening, sidestepping Blitz's immaturity problem entirely rather than waiting for Blitz to
  solve it.

**Resolution reached on the second point (diff overhead vs. recomposition benefit):** the two
diffs are not the same diff and do not compete for the same work:

- **Dioxus's VDOM diff (Rust side)** computes *what changed* at the level of "which mutations need
  to be sent across the boundary" — this still happens, unavoidably, and is a real cost.
- **Compose's recomposition (Kotlin side)**, once a mutation arrives, still gets to apply its own
  optimizations (snapshot state, skippable composables) to decide *how little of the Compose tree
  actually needs to re-render* in response to that mutation. `dioxus-compose`'s own README example
  of this: a theme/dark-mode change becomes a single `SetTheme` mutation plus one
  `CompositionLocal` invalidation, rather than an O(nodes) storm of individual `SetProp` calls —
  i.e., Compose's recomposition efficiency on the receiving end is preserved regardless of what
  produced the mutation stream.

So the accurate framing is: Dioxus-side diffing determines the cost of deciding what to send;
Compose-side recomposition determines the cost of applying it once received — additive costs at
different layers, not competing claims to the same optimization. And per the measured numbers in
§1.11, the Dioxus-side cost (13.3 µs against an 8.33 ms 120 Hz frame budget, i.e., ~0.16% of the
budget) was, at time of reading, nowhere near a bottleneck on the host side specifically.

### 1.13 "So can this beat webviews entirely, then?" — the general future-of-Blitz discussion

A broader question was asked and answered: given Blitz's uncertain future (per the internal
Dioxus Labs debate found in §1.8), what does that mean for the field generally, and for
`dioxus-compose` specifically?

**Conclusion reached:** Blitz's uncertain trajectory does not threaten `dioxus-compose`'s reason to
exist — if anything it reinforces it, because `dioxus-compose` does not depend on Blitz ever
maturing; it bypasses the need entirely by using Compose today. The trade `dioxus-compose` accepts
instead is a different dependency risk: it now depends on **Compose Multiplatform's own roadmap**
(§1.14) and on **GraalVM's native-image ecosystem** (also §1.14), rather than on Blitz's.

The broader field was assessed as multiple simultaneous, differently-tradeoffed bets (Blitz's
eventual pure-Rust stack; `dioxus-compose`'s Compose-delegation approach; Flutter's mature
Skia-based approach; Tauri v2's optimized-webview approach) that likely coexist rather than one
outright displacing the others, because each optimizes for a different point in the tradeoff
space.

### 1.14 Does `dioxus-compose` become dependent on Compose Multiplatform's own roadmap?

Explored directly and confirmed: **yes, substantially**, across several concrete dependency
points, each traced to something specific in what was read from the repository:

- **Platform coverage** — `dioxus-compose`'s own README explicitly lists iOS as "designed, not
  implemented" and Web as "designed, feasibility open." Ember's timeline for anything beyond
  desktop inherits this.
- **Rendering quality/bugs** — anything delegated to Compose (text shaping, IME, accessibility)
  inherits whatever bugs or platform-maturity gaps Compose Multiplatform itself has on a given
  target; `dioxus-compose` does not independently fix these, by design (`C5` forbids bypassing
  Compose-grade quality, which also means not patching around Compose's own gaps outside Compose).
- **API stability** — Compose's internal APIs (snapshot state system, `Modifier`, layout APIs)
  changing requires the Kotlin renderer side of `dioxus-compose` to track those changes.
- **The native-compilation toolchain itself** — the GraalVM native-image trick specifically depends
  on Compose Desktop/Skiko's AWT-based implementation continuing to work the way it currently does;
  if JetBrains changes that internal implementation (they have no particular reason to prioritize
  native-image compatibility, since their own target is "runs correctly on a JVM," not "AOT
  compiles cleanly"), the macOS shims described in §1.11 could break.
- **GraalVM's own macOS AWT gap** — the underlying upstream GraalVM issue (AWT unsupported on
  Darwin) is a **GraalVM roadmap** dependency, currently worked around by a third party (BellSoft
  Liberica NIK), which is itself a dependency of a dependency: if BellSoft stops maintaining that
  patch, or GraalVM's own direction shifts, this specific workaround could stop working.

**Overall framing reached:** `dioxus-compose` lives at the intersection of two large upstream
projects (Dioxus and Compose Multiplatform), which is architecturally the same pattern **Redwood**
(Cash App) and **Glance** (Google) already use successfully (§1.15) — so the pattern itself is
validated by precedent — but it does mean ongoing maintenance cost tracking both upstreams'
roadmaps is an accepted, permanent cost of this design, not a one-time integration effort.

### 1.15 Redwood and Glance: the precedent for `dioxus-compose`'s pattern

Investigated directly (web search) because the user asked what these two projects, mentioned in
passing, actually are.

**Redwood (Cash App / Block).** A Kotlin/Compose framework for building Android, iOS, and web UI,
whose explicit stated values (per Cash App's own engineering blog and a Jake Wharton
conference-talk description) are: (1) render using each platform's actual native UI toolkit —
"native UI is the best UI"; (2) reuse the rest of the app's existing components/styles rather than
redefining them; (3) use a language (Kotlin, chosen specifically because it compiles to JVM
bytecode, native code via LLVM, and JavaScript) with tooling engineers already know; (4) allow
incremental, non-all-or-nothing adoption. Structurally: Compose is used purely as a state-management
and UI-node-construction engine; the actual widgets it produces are mapped, per platform, onto that
platform's real native views (UIKit on iOS, Android Views on Android) — not any Compose-specific
rendering. Explicitly contrasted with React Native in the source material: "React Native uses the
native UI toolkit of each platform, but requires JavaScript and is always chasing compatibility
with each platform's new features" — Redwood's pitch is the native-toolkit benefit without the JS
dependency. Uses a **schema** concept (a `data class` defining each UI widget's shape, e.g. a
`TextInput` with `state`/`hint`/`onChange` fields) as the single source of truth connecting Compose
authoring to each platform's native rendering — structurally identical in purpose to
`dioxus-compose`'s own `schema.rs`.

**Glance (Google).** Distinct in purpose from Redwood: Glance lets developers write Android home
screen widgets and watch faces using Compose syntax (`@Composable`, `Column`, `Text`, etc.), while
the actual rendering target is **`RemoteViews`** — a separate, much more constrained Android system
required for widgets because they render out-of-process, for security/performance reasons, and
cannot use an arbitrary View tree the way a normal app screen can. Glance's Compose code is
compiled/interpreted down into `RemoteViews` calls; Compose itself never actually renders the
widget directly. This is the precedent `dioxus-compose`'s own README specifically cites ("Cash
App's Redwood and Jetpack Glance use the same pattern") for its "declarative authoring layer,
separate from actual rendering engine, connected via a schema/protocol" architecture.

**The generalized pattern, stated once for clarity:** all three projects (Redwood, Glance,
`dioxus-compose`) separate "the language/syntax used to *author* UI declaratively" from "the
system that actually *renders* it," connected by an explicit schema/protocol, rather than building
a new renderer from scratch. This is presented as the considered, precedented alternative to the
Blitz-style "build a new renderer from scratch" approach, and it's the lineage `dioxus-compose` —
and by extension Ember's launcher — sits in.

---

## Part 2 — From `dioxus-compose` to Ember: the IDE-specific design conversation

### 2.1 The actual goal, stated directly

The user's stated target: build an agentic IDE comparable to **Orca** or **Paseo**, i.e., an IDE
with an embedded AI agent. The specific complaint (echoing §1.1) that motivates going native rather
than accepting an existing web-based stack: existing stacks in this category are uniformly
web-based, and their *quality* (not merely their weight) was frustrating enough to motivate the
native route.

The user explicitly does **not** intend to build a new IDE/editor from scratch. Instead: run VS
Code with its `--serve-web` option on a server, and surface that from a local client — i.e., use
VS Code itself as the actual editing engine, and build native tooling around/alongside it rather
than replacing it.

### 2.2 First framing: can Dioxus/Blitz render HTML content directly, e.g. embedding VS Code Web?

Before the launcher/editor-window split was introduced, the question was asked in its simplest
form: can `dioxus-compose` itself render arbitrary HTML/CSS content, the way Blitz can?

**Answer given:** no, and this is structural, not a missing feature. `dioxus-compose` deliberately
never touches HTML/CSS at all — its widget schema is Compose-native concepts (`Column`, `Row`,
`Box`, `Text`, `Button`, directly mirroring Compose's own widget vocabulary) expressed through
`rsx!` syntax, not `div`/`span`/`<p>`. There is no HTML parser or CSS engine anywhere in its
pipeline; mutations are encoded as fixed-layout binary records interpreted directly as Compose
widget operations. This is presented as the precise opposite of Blitz's design (Blitz *is* an
HTML/CSS engine; `dioxus-compose` *has none at all*). Extending `dioxus-compose` to parse and
render arbitrary HTML would mean building an HTML/CSS engine from scratch inside it — reproducing
exactly the burden `dioxus-compose` was built specifically to avoid (per §1.11's own stated
motivation) — and would cut against its own `C4` non-negotiable ("UI is authored declaratively in
Rust; Kotlin is a renderer implementation detail," not "Kotlin renders arbitrary web content").

**The suggested alternative at that point**, before the actual use case (embedding VS Code Web
specifically) was disclosed: for genuinely web-requiring content, partially embed a real webview
natively (`WKWebView`/`WebView2`) — acknowledged at the time as being in direct tension with
`dioxus-compose`'s own `C1` ("no webview") principle, requiring either an explicit, scoped,
opt-in exception, or reconsideration of whether that content belongs in the Compose-authored tree
at all.

### 2.3 The specific case disclosed: VSCode Web

Once the user specified the actual content in question — **VS Code Web / code-server**, not
generic web content — the analysis changed substantially, because VS Code Web is not comparable to
arbitrary HTML:

- It is a full JavaScript application built on **Monaco** (a complex, heavily interactive text
  editor), potentially involving **WebSocket**-based real-time communication (for a `code-server`-
  style deployment) and browser-specific APIs (Service Workers, IndexedDB, an extension system).
- **This rules out any HTML-parsing-based approach** (the "parse HTML into a Compose tree" idea
  floated generically in §2.2) outright — Monaco is a complete JS application, not markup that
  could be reinterpreted as Compose widgets. It requires an actual JS engine, full stop.
- **This also rules out Blitz as a candidate**, directly connecting back to §1.9's finding: Blitz
  has no JS engine, so it cannot run Monaco either, regardless of Blitz's CSS/layout maturity.

**Conclusion at this point:** for VS Code Web specifically, a real webview embed is, practically
speaking, the only viable option — there is no HTML-reinterpretation or Blitz-based path around it.

### 2.4 Reconciling this with `dioxus-compose`'s no-webview principle

The needed reconciliation, worked through directly: rather than treat `C1` as violated wholesale,
narrow it to an explicit, opt-in exception — "the default widget tree is entirely pure Compose" is
preserved as the baseline rule; a webview is allowed only where a specific feature explicitly
requires JS execution, invoked deliberately rather than being the default rendering path for
anything. The observation was made that this is structurally similar to how VS Code Desktop itself
is an Electron (web) app overall, but the *editor surface specifically* is what actually needs web
tech — i.e., scoping the exception to where it's actually needed, rather than treating "any web
tech anywhere" and "the whole app is a webview" as the same thing.

**Honest caveat stated at the time, carried forward:** needing VS Code Web at all means accepting,
for that specific screen/window, that the exact weight-avoidance benefit motivating the native
route in the first place is given up *there*. The framing settled on: "the whole app stays light;
the window/screen that specifically needs an editor gets to be heavy, deliberately and
acknowledged, not accidentally."

### 2.5 The actual product shape: JetBrains Gateway-style separation

The user then clarified the real intended UX, which reframed the entire embedding question:

- The webview (VS Code Web) and the native launcher are **not visually combined in one window** —
  they are entirely separate windows/processes.
- Opening the app shows a launcher: a list of **projects**, each with its assigned **server
  computer(s)**, plus a way to jump directly into a project's **AI agent conversation history**.
- Clicking a server opens that project's VS Code Web instance, served from that server, in a **new
  window**.
- The result should feel to the user like opening VS Code Desktop normally, once that window is
  open.

This is explicitly identified as closer to **JetBrains Gateway**'s model than to VS Code's own
single-window model: a light front door, heavy per-project sessions opened on demand, minimal
coupling between them.

**Why this reframing matters, stated explicitly:** it resolves the C1 tension far more cleanly
than the "webview embedded inside a Compose-authored tree" framing from §2.2/§2.4 did. A webview
that is an *entire separate OS window*, rather than a widget composited inside another framework's
render tree, is architecturally closer to "launching a second, independent application" than to
"a UI framework rendering a foreign element type." No webview-compositing widget, no interop layer
inside `dioxus-compose`'s schema, is needed at all — the launcher only needs the ability to ask the
OS to open a new window pointed at a URL, which is a much smaller, much less architecturally
invasive capability.

### 2.6 Can this still have VS Code Desktop-like custom titlebar and tab-drag-to-new-window behavior?

Investigated directly, because achieving a "feels like real VS Code Desktop" experience specifically
requires it.

**How VS Code Desktop achieves this today:** through Electron-specific native window control APIs
(`BrowserWindow`, drag-region configuration, multi-window process management) — genuinely
Electron-only capabilities, not standard browser/web-platform features.

**Custom titlebar:** achievable, with caveats. VS Code Web draws its own in-page
"titlebar"-looking UI (menu bar, tab strip) inside the page itself — it is not an OS-native
titlebar, it's a web element styled to resemble one. By hiding the actual OS titlebar on the
webview-hosting native window (`decorations: false`-equivalent, platform-specific — macOS's
`titleBarStyle`, an analogous Windows treatment) and letting VS Code Web's own in-page header
occupy that space, a visually convincing approximation is achievable — a well-established trick
used by Electron/Tauri apps generally, not something novel needed here.

**Tab drag-to-detach-into-new-window:** this is **not achievable through VS Code Web at all**,
and this is a hard architectural fact, not a configuration gap. VS Code Web's own codebase simply
does not contain this feature — it is implemented in VS Code Desktop specifically using Electron's
`BrowserWindow` APIs, which have no equivalent inside a browser/webview context (browsers deliberately
prevent a webpage from spawning genuinely independent OS windows this way, for security reasons).
**Workaround identified:** intercept the tab-drag gesture at the JS/DOM level inside the webview
(mousedown → drag-past-threshold → mouseup outside the tab strip), and on detection, hand off to
the *native* shell — which then opens an actual new native window with its own new webview,
pre-loaded to the detached file/state. This produces a similar *outcome* (a new window opens with
that file) but not the same *mechanism* (VS Code Desktop's true same-process tab split) — the
distinction was made explicit and accepted as a known, acknowledged gap rather than glossed over.

### 2.7 Can the launcher↔webview communication be made JSI-like (synchronous, zero-copy)?

The user asked directly whether the communication channel needed for §2.6's workaround (and for
launcher↔editor-window communication generally) could be made "JSI-style" — referencing React
Native's JavaScript Interface, known for eliminating the traditional React Native "bridge"'s
serialization/queueing overhead via synchronous, same-memory-space calls.

**Answer: no, not in the same sense, and this is explained structurally, not dismissed.**
JSI's actual mechanism of being fast is that the JS engine and native code share the same process
and memory space — genuine C++ object references passed directly to JS, real synchronous function
calls, no queue, no serialization. This precondition does not hold here: a webview (VS Code Web's
JS) and the native shell (`dioxus-compose`'s Rust/Kotlin) are **different processes** with
**separate memory spaces**, unconditionally, by webview architecture — this is not something
Ember's design choices could change even in principle. So "JSI-style, literally" (same-process,
zero-copy) is ruled out categorically, not as a matter of difficulty.

**What is actually available, ordered fastest-to-slowest:**

1. **Native webview bridge APIs** — closest available approximation to JSI, given the
   process-boundary constraint. macOS: `WKScriptMessageHandler` (JS calls
   `window.webkit.messageHandlers.<name>.postMessage(...)`, native side registers a callback) —
   asynchronous but low overhead, lightweight (JSON-level) serialization. Windows: `WebView2`'s
   `postMessage`/`AddHostObjectToScript` — the latter exposes a COM host object JS can call methods
   on almost directly, the closest thing to JSI available on that platform.
2. **Local WebSocket** — more portable across platforms with uniform code, but strictly slower
   (socket round-trip plus serialization on every message) than option 1.
3. Other variants (custom URL schemes, etc.) — essentially fall into the same two categories above.

**Applied to the actual use case (tab-detach events, occasional window-management actions):**
these are low-frequency, human-triggered events (someone dragging a tab occasionally), not a
high-throughput real-time sync channel — so the speed difference between options 1 and 2 would not
be perceptible in this specific use case; JSI-grade latency is unnecessary here regardless of which
option is picked. The user, on hearing this, settled on **option 1 (native bridge APIs)** as the
choice, specifically because it avoids needing to stand up a local WebSocket server at all, not
primarily because of a latency requirement.

### 2.8 The core question: which side has to be official VS Code, and can the other side be substituted?

The user posed this as two explicit sub-questions, given the earlier stated goals of (a) keeping
the official VS Code Marketplace fully compatible, (b) needing to modify the VS Code *UI*
regardless, and (c) preferring, if at all possible, to avoid shipping a full Node.js VS Code server
on the server side:

1. Which side is unconditionally required to be the official VS Code runtime?
2. Can the server side's responses be served by a Rust or Python server instead, without breaking
   extension functionality?

This was investigated via direct web search (query: "VS Code architecture extension host vs web
server process separation") before answering, rather than answered from assumed prior knowledge.

**Answer to (1): the Extension Host, unconditionally.** VS Code's own architecture documentation
states plainly that extensions run in a separate process, the Extension Host, specifically so that
misbehaving extension code cannot affect the core editor's stability or startup time — and that
this process "is a Node.js process and it exposes the VS Code API to extension writers." Every
marketplace extension is Node.js code written against that exposed API — an internal, evolving,
effectively unpublished-as-a-stable-spec RPC protocol connecting the Extension Host to the
Renderer via proxy objects on each side. There is no substitute implementation of this that
would not itself become a second, perpetually-diverging reimplementation of VS Code's most
actively-developed internal surface — so this side is non-negotiable.

**Answer to (2): only very partially, not for the Extension Host itself.** VS Code's actual
multi-process model, confirmed via search, separates: Renderer (Workbench UI), **Extension Host**
(runs extensions — confirmed unconditionally required, per above), Language Servers/Debug Adapters
(already separate processes, communicating via stdio/JSON — LSP/DAP — largely orthogonal to this
question since they're decoupled from VS Code core by design already), Pty Host (terminal
management), Shared Process (background tasks). The search also surfaced the `ExtensionHostKind`
enum distinction directly relevant here: `LocalProcess` (Node.js child process, default for
desktop), `LocalWebWorker` (browser-only APIs, the only option on plain vscode.dev), and
**`Remote`** (a Node.js process on a *separate machine*, reached over the network via
WebSocket) — this last one being architecturally identical to what Ember's server-side setup
already requires, meaning Ember's topology is not inventing a new relationship between Renderer and
Extension Host but reusing one VS Code already ships (standard Remote Development / SSH /
container / WSL support). Additionally, the concept of `extensionKind` (`ui` vs `workspace`) was
surfaced: `workspace`-kind extensions (most extensions needing filesystem/network access — the
majority of what a project-hosted-on-a-server scenario would need) specifically run in the remote
(server-side) Extension Host, reinforcing that the server-side Node.js Extension Host is not an
optional or lightly-loaded component for Ember's actual use case.

**What genuinely might be substitutable:** purely the **static asset serving** slice of what
`--serve-web` bundles — the HTML/JS/CSS payload delivery — which is, in the abstract, an ordinary
HTTP-serving problem with no inherent Node.js requirement. But this is explicitly caveated as
likely a modest optimization (reducing load on the Node.js side, enabling local caching) rather
than a path to removing Node.js from the server picture altogether, since the Extension Host — the
actually heavy, actually load-bearing piece — remains Node.js regardless. Whether this split is
even cleanly separable in `--serve-web`'s actual implementation was flagged as unverified,
requiring a source-level read, not something confirmed at the time of the conversation.

**Summary answer given:** server side = official VS Code (specifically its Extension Host)
required, non-negotiable; front end (Renderer/Workbench UI) = substitutable in principle, though
(per §2.9–§2.11 below) substituting it fully turns out to carry much larger costs than initially
apparent.

### 2.9 Follow-up: if Monaco (the Renderer/Workbench UI) is removed, do extensions still work?

Having established the Renderer side is *architecturally* substitutable (§2.8), the user asked the
natural next question directly: practically, does removing Monaco break marketplace extensions?

**Answer: depends entirely on which of several structurally different extension categories is in
question** — this taxonomy was worked out directly in response and is the single most detailed
piece of technical analysis in the whole conversation (fully reproduced, with more precision, in
`IMPLEMENTATION.md` §3–4 of this repository's own documents; summarized here for background
completeness):

1. **Pure Extension Host logic** (linters computing diagnostics, formatters, Git integration logic,
   debug adapter clients) — these compute results and report them via API calls; they never touch
   rendering directly. **Unaffected by removing Monaco.**
2. **Editor-surface overlay extensions** — diagnostics *display*, CodeLens, Hover providers,
   inline completion providers (the Copilot-class category). These report **structured data**
   (line/column ranges, markdown content, ghost-text strings) via API calls; the Renderer (Monaco,
   currently) is responsible for actually drawing that data positioned correctly against the live
   text layout. **Breaks without Monaco specifically because the *positioning/drawing* logic
   currently lives in Monaco** — but a replacement Renderer that (a) implements the same reporting
   contract and (b) does correct text-layout-to-screen-position math itself would, in principle, be
   a legitimate drop-in replacement, extension-invisibly.
3. **Extensions that ship their own rendered content** — `vscode.window.createWebviewPanel`-based
   extensions (Markdown Preview Enhanced, Jupyter notebook cell rendering, GitLens' graph views,
   REST Client's response viewer). These aren't reporting data for a Renderer to draw at all —
   they are shipping a self-contained HTML/CSS/JS app inside a panel, by the API's actual design.
   **Removing Monaco is irrelevant to this category** — their dependency was never on Monaco, it's
   on having *a* webview available for their panel, which is a separate question from "does the
   IDE's main editing surface use Monaco."
4. **Extensions calling `monaco.editor.*` directly** — a smaller set of extensions bypass the
   Extension-Host-mediated reporting APIs and call Monaco's own client-side API surface directly
   from within a webview context. These have a hard dependency on Monaco *as a specific
   implementation*, not on "any Renderer that behaves similarly" — no amount of
   protocol-compatibility in a replacement Renderer fixes this category. (Flagged, both in this
   conversation and carried into `IMPLEMENTATION.md`, as unsized — how much of the marketplace
   actually falls here is not known.)

**Summary conclusion:** "does removing Monaco break extensions" does not have one answer; it's
"no" for category 1, "yes unless carefully replicated" for category 2, "not applicable, this was
never about Monaco" for category 3, and "yes, unconditionally" for category 4. Given that category
2 specifically includes Copilot-class inline suggestions — directly core to what makes an *agentic*
IDE agentic — this category was identified as the one that matters most for Ember's actual purpose,
not an incidental detail.

### 2.10 "This sounds plausible though — could Monaco actually be fully reimplemented in Compose?"

The user pushed further, past "extensions mostly survive category-by-category," to the larger
question: could the whole Renderer genuinely be rebuilt in Compose, given that category 2's
analysis suggests it's not categorically impossible?

**Answer given, both sides argued honestly:**

**Why "yes, possible" is defensible:** the category-2 analysis itself supports it — extensions
fundamentally want structured data delivered and positioned correctly, not "Monaco specifically."
In principle this is a well-defined, protocol-compatible reimplementation target, not an
ill-defined one.

**Why the real difficulty is not "impossible" but "enormous, and a different kind of project":**
Monaco's actual difficulty is a decade-plus of accumulated edge-case handling — multi-language text
shaping (ligatures, variable-width Unicode), large-file virtual scrolling, large-diff rendering
performance, **IME** (explicitly connected back to §1.11's finding that `dioxus-compose` itself
was *founded* specifically because the Rust GUI ecosystem lacks Compose-grade IME — meaning
building a *new*, *separate*, *from-scratch* text-IME stack for Ember's editor would directly
contradict the reasoning that justified using Compose via `dioxus-compose` in the first place),
accessibility/screen-reader mapping, code folding, multi-cursor editing, bracket matching, a
minimap, and a full TextMate-grammar tokenization engine for syntax highlighting — each
individually a small-to-medium project on its own, and each one of category 2's overlay systems
(CodeLens, Hover, inline completions) needing its own correct coordinate-transform logic on top of
all that.

**The connecting insight, stated explicitly and treated as decisive:** this is recognized as
*exactly the same kind of bet* `dioxus-compose` itself made when choosing Compose over Blitz — "a
mature ecosystem component doesn't exist yet in our target stack, so borrow one instead of building
an immature version from scratch" (§1.11, §1.13). Rebuilding Monaco in Compose from scratch would
be choosing to *repeat* the mistake `dioxus-compose`'s own design explicitly avoids, at a
significantly larger scale, for the one editor feature (agent-driven inline suggestions) that
matters most for an agentic IDE's actual purpose.

**Conclusion reached, and this became the seed of `INTENT.md` D6/D8:** "possible" is not being
disputed; what's being flagged is that pursuing it *changes the nature of the project* from "build
an IDE shell around VS Code" to "build a Monaco competitor," a JetBrains-scale undertaking on its
own. The practical recommendation given at the time (which becomes `PROJECT.md`'s milestone
ordering and `INTENT.md` D8's non-negotiable-ordering decision): treat this as a genuine,
potentially worthwhile **long-term goal**, explicitly separated from and not blocking an actually
shippable **MVP** that starts from the wrapped-webview approach — and start, if pursued, with
category 2 (overlay/reporting extensions) rather than category 3 (self-shipped webview content,
which reimplementing Monaco does nothing to address anyway, per §2.9).

### 2.11 "But this would let the webview be fully removed, right?"

A further follow-up pressed on the actual payoff: if Monaco is successfully reimplemented in
Compose, does that mean the webview is eliminated entirely?

**Answer: the payoff is real but smaller than "fully removed," and this needed to be made
precise, because it directly follows from category 3's analysis in §2.9 rather than being a new
finding.**

Two distinct senses of "remove the webview" were disentangled:

1. **Removing VS Code Web/Monaco's own steady-state rendering as a webview** — achievable, if the
   Compose reimplementation (§2.10) succeeds, for category 1/2 extensions and for the core editing
   experience generally.
2. **Removing *every* webview any extension might ever want** — **not achievable**, categorically,
   because category-3 extensions (§2.9) — Jupyter, Markdown Preview Enhanced, GitLens graphs —
   ship their *own* HTML/CSS/JS by design; this was never a Monaco dependency to begin with, so no
   amount of Monaco-replacement work touches it. Eliminating this category would require asking
   every such extension's author to rewrite against a new, Ember-specific rendering API instead of
   the standard `createWebviewPanel` API — which directly violates the "extensions must work
   unmodified" goal stated back in §2.8/§2.9's framing, and was rejected on that basis rather than
   treated as a technical problem to solve.

**The realistic target this produced, stated as the actual recoverable goal (this became `E4` and
the substance of `IMPLEMENTATION.md` §5's "staged de-webview-ing plan"):** not "zero webview,
ever," but **"no standing/always-on webview — a webview exists only when, and only for as long as,
a specific extension that authored its own HTML is actively displaying its panel."** This was
identified as a materially different, more honest, and still genuinely valuable claim compared to
an unachievable "fully webview-free" claim — the shift from "eliminate webviews" to "shrink webview
scope from window-wide-and-standing to panel-local-and-on-demand" is the actual, defensible
payoff.

---

## Part 3 — How this background maps onto the produced documents

For an agent picking this up cold: the six other documents in this repository are the *distilled,
spec-form* output of everything above. Roughly:

- **`README.md`** — the elevator-pitch version of Parts 1–2's conclusion (§2.4–§2.11 especially).
- **`PROJECT.md`** — turns §2.10/§2.11's "MVP now, Monaco-replacement later, explicitly gated" into
  milestones M1–M6, and turns the unresolved threads (§2.8's "is the static-serving split actually
  clean," §2.6's "is tab-detach emulation good enough," DarkPyonix's own protocol shape) into
  `PROJECT.md`'s Q1–Q5.
- **`docs/INTENT.md`** — turns each numbered resolution above (§2.5's window-split, §2.6's
  wrap-don't-fork-plus-emulate-detach, §2.7's bridge-API choice, §2.10/§2.11's staged Monaco
  approach) into decisions D1–D9, each with the rejected alternative that was actually considered
  and set aside.
- **`docs/SPEC.md`** — turns the concrete, testable claims embedded in the above (the tab-detach
  gesture-detection logic of §2.6, the bridge message schema implied by §2.7, the extension-category
  acceptance criteria implied by §2.9) into `FR-*`/`NFR-*` IDs.
- **`docs/ARCHITECTURE.md`** — turns §2.5's window/process split and §2.8's VS Code process-model
  findings into the actual topology diagram and the table of which VS Code process is and isn't
  substitutable.
- **`docs/IMPLEMENTATION.md`** — turns §2.8–§2.11's extension-taxonomy analysis into the full
  category 1–4 breakdown with acceptance criteria, and §2.11's "shrink scope, don't eliminate" into
  the staged de-webview-ing table.

Nothing in Parts 1–2 above introduces a requirement that isn't already reflected in one of those
six documents; this document's job is solely to preserve the *reasoning path* that produced them,
for anyone who needs to evaluate whether that reasoning still holds before extending or revising
the conclusions.
