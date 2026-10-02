# INTENT.md — DarkPyonix Ember

> **Revised 2026-10-03.** The 2026-09-22 version of this document framed Ember as "a native
> launcher plus wrapped VS Code Web windows." The user's 2026-10-03 brief moved the centre of the
> product to **agent conversations that live on one main server and move between computers**.
> VS Code is still here, but as the optional coding window you open *from* a conversation, not as
> the thing the product is organised around. The earlier VS Code analysis is kept; see
> `IMPLEMENTATION.md` and `BACKGROUND.md`.
>
> **Marking convention.** Every decision below is tagged with where it came from:
> - **[user]** — stated by the user; quoted where the wording matters.
> - **[provisional]** — a team proposal the user has not confirmed. Treat it as a working
>   assumption that may be overturned, not as settled.

## Motivation

Three frictions, each from the user's own day-to-day work, define what Ember is for.

**1. Conversations are stuck on the machine that started them.** A coding CLI such as Claude Code
writes its transcript to the local disk of whatever computer it ran on. Moving a piece of work from
one computer to another means moving files around by hand, and in practice the conversation is
abandoned and restarted. [user] Ember keeps **every conversation on one main server** — a personal
Raspberry Pi or Mac mini — so that a conversation is no longer a property of a computer.

**2. Agents from different vendors cannot talk to each other.** Two Claude sessions can coordinate,
but only on the same machine and the same account; across machines that means pasting through the
Claude web app, and across accounts it is impossible. Claude and Codex cannot talk directly at all —
today the workaround is a shared file both agents poll. [user] Ember gives agents **a direct
channel to each other (A2A)** across models, machines and accounts.

**3. A remote machine's view of the network is hard to borrow.** When developing on a remote
server, you sometimes need to open a web page *as that server sees it*. Today that means SSH
port forwarding used like a VPN. [user] Ember opens **a browser session that egresses from the
chosen computer**, with its IP, while the browser's own data (cookies, logins, history) stays on
the main server — so moving between computers feels like using one computer.

There is a fourth, older motivation that the main-server model also addresses: running many CLI
agents locally makes a laptop slow. Moving the agent runtimes and transcripts to the main server
leaves the local machine with only a thin execution daemon (**ember node**) plus whatever the tools themselves
(builds, tests) cost. [provisional — the split between memory pressure and build CPU has not been
measured.]

The concept, in the user's words: **"한 인공지능이 작업 컴퓨터를 이동해가면서 작업하는 형태"** —
one AI that moves between work computers as it works.

## The shape of the product

- **A project is a company; computers are its branch offices.** [user] A project has computers
  assigned to it, and work for that project can happen at any of them. This is what separates
  Ember from agent multiplexers that treat a project as a folder on one machine.
- **Conversation first, code second.** [user] The main screen lists projects; each project shows
  its running conversation sessions and whether each has finished; the computer list sits at the
  bottom. Opening a conversation is the primary action. A coding window is opened only when needed,
  from an **"Open IDE"** button at the top right of the conversation view.
- **Ember launches other IDEs too.** [user] "Open IDE" can launch Ember's own IDE window, VS Code,
  or JetBrains Gateway.
- **The overall UX follows JetBrains Gateway**: a light front door, heavier per-project sessions
  opened on demand. [user]

## Components

The `darkpyonix-ember` repository covers these deliverables. [user]

| Component | What it is |
| --------- | ---------- |
| **ember** | The multiplatform client: everything described in this document. Its IDE window reaches files mainly through VS Code's own `serve-web`. For Android and iOS, where there is no Node, it also supports a `serve-web`-compatible Rust backend, or no backend at all with direct local access through web APIs (the `vscode.dev` model). Hosted for outside access through a dedicated manager. |
| **vscode-darkpyonix** | VS Code extension rendering DarkPyonix notebook files (`.py`, `.pynb`). Installed by default. |
| **vscode-darkpyonix-theme** | VS Code theme extension in the DarkPyonix Ember (phoenix) design language. Installed by default. |
| **intellij-darkpyonix** | IntelliJ / PyCharm plugin rendering DarkPyonix notebook files (`.py`, `.pynb`). Installed by default. |

The kernel, manager and hub APIs belong to `darkpyonix-core` and are linked from there, not
redefined here: `darkpyonix-core/docs/ARCHITECTURE.md`, `docs/api/manager.openapi.yaml`,
`docs/api/hub.openapi.yaml`, `docs/PROTOCOL.md`, `docs/FORMAT.md`. Ember's own documents define
the main server's **conversation, account, computer, shell-wrapping and A2A** APIs. [provisional
boundary, agreed with the darkpyonix leader]

## Non-negotiables

| ID | Constraint | Source |
| -- | ---------- | ------ |
| **E1** | The client's launcher and conversation screens never contain a webview, under any circumstance. They are built on `dioxus-compose`. The IDE window is the one place a webview may exist. | [user, 2026-10-03] |
| **E2** | A wrapped agent CLI (Claude Code, Codex, Antigravity, OMP (oh-my-pi), …) keeps its native behaviour. Ember adds around it — A2A, computer switching, browser — and never patches or reimplements the agent itself. | [user]: "본연의 동작을 보존해주면서 에이전트간 소통 기능만 추가" |
| **E3** | All conversations, all agent processes and all account credentials live on the main server. A computer is a place where tools run; it is never the system of record for a conversation. | [user] |
| **E4** | VS Code is wrapped, never modified: Ember does not patch VS Code's source and never reimplements the Extension Host. Which extension marketplace applies follows the chosen VS Code runtime (Open VSX for OSE, the Microsoft Marketplace for the official build). | [provisional — narrowed from the 09-22 "official marketplace required"; see D10] |
| **E5** | Inside the IDE window, a webview is scoped to the smallest region that needs it once the editor-core work of `IMPLEMENTATION.md` lands. Until then the IDE window as a whole is the acknowledged exception. | [kept from 09-22] |
| **E6** | A Compose-native editor core, if ever built, must not diverge from VS Code's behaviour; VS Code is correct by definition where the two disagree. | [kept from 09-22] |

## Decisions

### D1 — A conversation session is the top-level object; its computer is a changeable attribute

**Decision.** A conversation session belongs to a project, not to a computer. Which computer it is
currently executing on is an attribute of the session that can change during the session's life.
The 09-22 model — one project, one assigned server — is the special case of a session that never
changes computer.

**Source.** That a session is not bound to a computer and must be able to move between computers
is [user]. Making the session top-level and "current computer" an attribute is [provisional]; it
was chosen because it survives every open answer to Q1–Q3 without restructuring.

**Rejected alternative.** Binding each session permanently to one computer. Simpler — no stale
observations to manage — but it contradicts the core concept.

### D2 — Conversation first; the IDE is something a conversation opens

**Decision.** [user] The main screen is projects → conversations (with completion status) →
computers. The IDE is launched from the conversation view's "Open IDE" button and can be Ember's
own IDE window, VS Code, or JetBrains Gateway.

**Why.** An agentic workflow spends most of its time watching and steering agents, not editing.
Putting the editor at the centre, as the 09-22 design did, made the heavy surface the default one.

### D3 — One main server owns conversations, shells and accounts

**Decision.** [user] A single main server — a personal Raspberry Pi or Mac mini — runs every agent
CLI, stores every transcript, and manages every account. Computers connect to it; it does not
connect to a computer for a conversation's history.

**Implementation language.** [user, 2026-10-03] ember server is written in Rust ("ember server는
rust로 하면 된단다"). The Python transcript parsers in `proxy/dpx/agents/` stay where they are; the
Rust server's parsers share test vectors with them so both read history identically.

**Why.** It is the direct fix for motivation 1. It also gives, for free, sessions that survive the
local machine being closed (tmux for agents), several computers attaching to one session, and
search across all sessions in one place.

**Known cost.** [provisional analysis] A transcript records observations of a particular computer —
file contents, command output, absolute paths, the OS and toolchain. When a session moves, many of
those observations become false on the new computer, and an agent that trusts them will edit files
based on contents that are no longer there. How Ember invalidates them is open (Q4). Source code
read by the agent also ends up on the main server; acceptable for a self-hosted server, but it
changes if Ember is ever offered as a hosted product.

### D4 — Agents are wrapped at the shell boundary, and their tools run on the chosen computer

**Decision.** [user, original definition] Ember wraps the shell each CLI sees, so that CLIs running
on the main server behave as if their actions happen on the designated computer. The CLI process and
its transcript stay on the main server; its tool calls (file reads and writes, commands) are carried
out on the session's current computer and their results streamed back.

**Why this boundary.** It is the layer every CLI shares, which is what lets E2 hold: no CLI needs to
know it is being wrapped.

**Open.** The exact interception point per CLI (shell, PTY, filesystem, tool protocol) is an
implementation question to be answered per agent, against each CLI's actual behaviour — see Q6.

### D5 — Agents talk to each other directly (A2A), across models, machines and accounts

**Decision.** [user] Ember provides a messaging channel between agent sessions that works between
different vendors (Claude ↔ Codex), between sessions on different computers, and between sessions
under different accounts. It is the only behaviour Ember adds to a wrapped agent's conversation.

**Why.** Motivation 2. Because every session already runs on the main server (D3), the channel is
local to the server, regardless of which computers the sessions are working on.

**Rejected alternative.** A shared file that agents read and write. It is what users do today, and
it is the limitation this decision exists to remove.

### D6 — A remote browser that egresses from the chosen computer, with its data on the main server

**Decision.** [user] Ember can open a browser session whose network traffic leaves from a chosen
computer, so that pages see that computer's IP and network. The browser's profile data — cookies,
sessions, storage — is kept on the main server, so switching computers keeps the same logged-in
browser. Agents can use the same browser (agent browser use).

**Why.** Motivation 3, and it extends "one AI moving between computers" to the browser: one
browser identity, many vantage points.

### D7 — Many accounts per agent, with usage routing

**Decision.** [user] The main server holds several accounts each for Claude Code, Codex and
Antigravity. It can route usage between accounts by policy or by configuration, and a new
conversation can be started under a chosen account. Signing in with OpenAI is supported, with a
page that lets ChatGPT usage be consumed as well as Codex token usage.

### D8 — Computers connect peer to peer, with darkpyonix.dev relaying the hole punch

**Decision.** [user] The main server connects to each computer peer to peer, through either a
tunnel implemented in Rust or an existing mesh product ("tailcat" in the brief — `github.com/tailscale/tailcat`,
Tailscale's data plane as a standalone Go library and CLI, BSD-3). The DarkPyonix central server, `darkpyonix.dev`, relays NAT hole punching
so that, as with Paseo, users do not have to think about connectivity.

**Consequence.** This also settles the HTTPS problem `proxy/` has had for phones and tablets
(`proxy/docs/BACKGROUND.md` §7-1), whichever transport is chosen — [provisional].

### D9 — The client is native (dioxus-compose); the IDE window is wrapped VS Code Web

**Decision.** [user, 2026-10-03] The launcher and the conversation screens are built on
`dioxus-compose` with no webview (E1). The IDE window is a webview running VS Code Web through
Ember's wrapping layer, whose current implementation is `proxy/` (merged in #1).

**Tauri.** [user, 2026-10-03] The Tauri scaffold has been deleted ("Tauri 스캐폴드가 왜 필요해?
지워."). The Tauri parts of `docs/design/INTEGRATION.md` are marked obsolete; its VS Code runtime
choice (D10) and mobile WebView notes still apply to the IDE window.

### D10 — Two VS Code runtimes: OSE by default, the official build as an option

**Decision.** [provisional — from `docs/design/INTEGRATION.md`, user-committed 2026-10-02; the
brief itself names no build] The IDE window supports two VS Code runtimes:

| | OSE | VSC |
| - | --- | --- |
| Build | Compiled by DarkPyonix from the MIT source | The user's own installation of Microsoft's build |
| Marketplace | Open VSX | Microsoft Marketplace |
| Default | Yes | — |
| Licence | MIT | Microsoft's licence, the user's responsibility |

**Consequence for E4.** The 09-22 rule "the official marketplace must work" became "the marketplace
matching the chosen runtime works" — the OSE default cannot reach the Microsoft Marketplace.

**Mobile without Node.** [user] On Android and iOS the IDE window must work without Node, through a
`serve-web`-compatible Rust backend or by direct local access through web APIs. This does **not**
reimplement the Extension Host (E4): without Node there is no Node extension host at all, so only
extensions that ship a web build (VS Code's browser web-worker extension host, as on `vscode.dev`)
run there. Extensions that need Node require a VS Code server on a computer.

### D11 — The 09-22 VS Code decisions still hold, inside the IDE window

These were decided for the IDE window and are unchanged in substance. Their full reasoning is in the
09-22 version of this document (git history) and in `IMPLEMENTATION.md`:

- **Wrap, don't fork.** Behaviour changes to VS Code Web come from CSS/DOM injection and a bridge on
  top of an unmodified build. `proxy/` is that layer.
- **Tab detach is emulated** by detecting the gesture in the webview and opening a new window with
  the file's state, not ported from Electron.
- **The webview ↔ native bridge** uses each platform's own message-handler API
  (`WKScriptMessageHandler`, WebView2 `postMessage`) behind one Rust trait.
- **No Monaco replacement before the product ships.** A Compose-native editor core is the long-term
  M-series ambition, gated behind a working product; it starts, if ever, with overlay-style
  extensions (diagnostics, CodeLens, hover, inline completions), never with webview panels.

### D12 — DarkPyonix is a service Ember talks to, not a dependency it vendors

**Decision.** Unchanged from 09-22. The DarkPyonix kernel is its own process with its own contract,
now defined in `darkpyonix-core` (`docs/PROTOCOL.md`, `docs/api/*`). Ember depends on that
contract, not on the kernel's internals.

### D13 — `proxy/`'s hub is transitional; its agent adapters are kept and moved to the main server

**Decision.** [provisional] `proxy/dpx/hub/` puts several computers on one home screen by having
each computer report the transcripts it holds locally (`~/.claude`, `~/.codex`). E3 replaces that
model: transcripts live on the main server, not on the computers. The hub is marked transitional.
The adapters in `proxy/dpx/agents/` — which parse Claude Code and Codex transcripts — stay useful
unchanged, because the main server is now where those transcripts are; they are kept and
relocated, not removed.

## Open questions

| ID | Question | Status |
| -- | -------- | ------ |
| **Q1** | How do a project's computers relate: copies of the same workspace (the same repository checked out on a Mac and a Linux box), machines with different roles (iOS builds on the Mac, GPU work on Linux), or simply "whichever computer I am at"? | Open — decides how much of a session's observations survive a move |
| **Q2** | Who moves a session: the agent (a `switch_computer` tool plus a routing policy, e.g. "needs CUDA → GPU box"), the user, or both? | Open — the concept suggests the agent, unconfirmed |
| **Q3** | When a session moves, does an open IDE window follow it, or stay with its computer? | Open |
| **Q4** | How are a transcript's computer-specific observations invalidated on a move? Proposal: split the transcript into a computer-independent part (intent, decisions, plans, conclusions) and a computer-specific part (file contents, command output, paths, environment, background jobs); record each file observation as (path, content hash, computer, time) and re-hash on the new computer so only changed files are flagged; keep the environment description in one replaceable block instead of appending; scope "read before edit" to a computer. | [provisional] proposal only |
| **Q5** | A job started on computer A when the session moves to B: kill it, keep it and notify on completion, or block the move? Proposal: keep it running and notify. | [provisional] |
| **Q6** | For each wrapped CLI, where exactly is the interception point — shell, PTY, filesystem or tool protocol — that keeps its native behaviour intact? Candidates per agent: the vendor's own headless protocol, or the Agent Client Protocol (ACP), which OMP exposes natively and Claude Code / Codex / Gemini reach through adapters. | **Claude Code:** `--print` stream-json with `--permission-prompt-tool stdio` (#14). **Codex:** `codex app-server` directly (#14); the maintained ACP adapter (`agentclientprotocol/codex-acp`) is itself a translation layer over app-server, so it can only lose detail (approval choices, turn steering, thread ids, usage), and the older one compiles Codex internals pinned to an old release, breaking FR-A1. Others: open. |
| **Q7** | Which transport: iroh, rustunnel, a tunnel written in Rust from scratch, or tailcat? | *[provisional — awaiting user]* **iroh 1.0**, behind a replaceable interface. tailcat was withdrawn after the user's objection (two apps on mobile). An own implementation is weeks rather than months for the basic path, since a failed hole punch falls back to the relay; it stays the replacement option. `rustunnel` is not a transport: it is a server-relayed tunnel with no hole punching, and AGPL-3.0, so it must not be linked into clients; it is only a reference for the hub's public HTTPS edge. Source: darkpyonix leader, core PROJECT Q1 (3ea7fb7). |
| **Q8** | Is the Tauri scaffold kept long-term, and for what? | **Closed — deleted** [user, 2026-10-03] |
| **Q9** | `proxy/`'s open items carry over: HTTPS for phones (likely resolved by D8), login being a thin shell, and the pre-distribution security holes in `proxy/docs/BACKGROUND.md` §7-3. | Open |
