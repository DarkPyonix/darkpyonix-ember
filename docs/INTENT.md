# INTENT.md: DarkPyonix Ember

> **Revised 2026-10-03.** The 2026-09-22 version of this document framed Ember as "a native
> launcher plus wrapped VS Code Web windows." The user's 2026-10-03 brief moved the centre of the
> product to **agent conversations that live on one main server and move between computers**.
> VS Code is still here, but as the optional coding window you open *from* a conversation, not as
> the thing the product is organised around. The earlier VS Code analysis is kept; see
> `IMPLEMENTATION.md` and `BACKGROUND.md`.
>
> **Marking convention.** Every decision below is tagged with where it came from:
> - **[user]**: stated by the user; quoted where the wording matters.
> - **[provisional]**: a team proposal the user has not confirmed. Treat it as a working
>   assumption that may be overturned, not as settled.

## Motivation

Three frictions, each from the user's own day-to-day work, define what Ember is for.

**1. Conversations are stuck on the machine that started them.** A coding CLI such as Claude Code
writes its transcript to the local disk of whatever computer it ran on. Moving a piece of work from
one computer to another means moving files around by hand, and in practice the conversation is
abandoned and restarted. [user] Ember keeps **every conversation on one main server** (a personal
Raspberry Pi or Mac mini), so that a conversation is no longer a property of a computer.

**2. Agents from different vendors cannot talk to each other.** Two Claude sessions can coordinate,
but only on the same machine and the same account; across machines that means pasting through the
Claude web app, and across accounts it is impossible. Claude and Codex cannot talk directly at all;
today the workaround is a shared file both agents poll. [user] Ember gives agents **a direct
channel to each other (A2A)** across models, machines and accounts.

**3. A remote machine's view of the network is hard to borrow.** When developing on a remote
server, you sometimes need to open a web page *as that server sees it*. Today that means SSH
port forwarding used like a VPN. [user] Ember opens **a browser session that egresses from the
chosen computer**, with its IP, while the browser's own data (cookies, logins, history) stays on
the main server, so moving between computers feels like using one computer.

There is a fourth, older motivation that the main-server model also addresses: running many CLI
agents locally makes a laptop slow. Moving the agent runtimes and transcripts to the main server
leaves the local machine with only a thin execution daemon (**ember node**) plus whatever the tools themselves
(builds, tests) cost. [provisional: the split between memory pressure and build CPU has not been
measured.]

The concept, in the user's words: **"한 인공지능이 작업 컴퓨터를 이동해가면서 작업하는 형태"**:
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
| **E1** | The client's launcher and conversation screens never contain a webview, under any circumstance. They are built on `compose-rust` directly; only the IDE screens use `dioxus-compose` (D9). The IDE window is the one place a webview may exist. | [user, 2026-10-03; toolkit split: user, 2026-10-26] |
| **E2** | A wrapped agent CLI (Claude Code, Codex, Antigravity, OMP (oh-my-pi), …) keeps its native behaviour. Ember adds around it (A2A, computer switching, browser) and never patches or reimplements the agent itself. | [user]: "본연의 동작을 보존해주면서 에이전트간 소통 기능만 추가" |
| **E3** | All conversations, all agent processes and all account credentials live on the main server. A computer is a place where tools run; it is never the system of record for a conversation. | [user] |
| **E4** | VS Code is wrapped, never modified: Ember does not patch VS Code's source and never reimplements the Extension Host. Which extension marketplace applies follows the chosen VS Code runtime (Open VSX for OSE, the Microsoft Marketplace for the official build). | [provisional, narrowed from the 09-22 "official marketplace required"; see D10] |
| **E5** | Inside the IDE window, a webview is scoped to the smallest region that needs it once the editor-core work of `IMPLEMENTATION.md` lands. Until then the IDE window as a whole is the acknowledged exception. | [kept from 09-22] |
| **E6** | A Compose-native editor core, if ever built, must not diverge from VS Code's behaviour; VS Code is correct by definition where the two disagree. | [kept from 09-22] |

## Decisions

### D1: A conversation session is the top-level object; its computer is a changeable attribute

**Decision.** A conversation session belongs to a project, not to a computer. Which computer it is
currently executing on is an attribute of the session that can change during the session's life.
The 09-22 model (one project, one assigned server) is the special case of a session that never
changes computer.

**Source.** That a session is not bound to a computer and must be able to move between computers
is [user]. Making the session top-level and "current computer" an attribute is [provisional]; it
was chosen because it survives every open answer to Q1–Q3 without restructuring.

**Rejected alternative.** Binding each session permanently to one computer. Simpler (no stale
observations to manage), but it contradicts the core concept.

### D2: Conversation first; the IDE is something a conversation opens

**Decision.** [user] The main screen is projects → conversations (with completion status) →
computers. The IDE is launched from the conversation view's "Open IDE" button and can be Ember's
own IDE window, VS Code, or JetBrains Gateway.

**Why.** An agentic workflow spends most of its time watching and steering agents, not editing.
Putting the editor at the centre, as the 09-22 design did, made the heavy surface the default one.

### D3: One main server owns conversations, shells and accounts

**Decision.** [user] A single main server (a personal Raspberry Pi or Mac mini) runs every agent
CLI, stores every transcript, and manages every account. Computers connect to it; it does not
connect to a computer for a conversation's history.

**Implementation language.** [user, 2026-10-03] ember server is written in Rust ("ember server는
rust로 하면 된단다"). The Python transcript parsers in `ember/proxy/dpx/agents/` stay where they are; the
Rust server's parsers share test vectors with them so both read history identically.

**Why.** It is the direct fix for motivation 1. It also gives, for free, sessions that survive the
local machine being closed (tmux for agents), several computers attaching to one session, and
search across all sessions in one place.

**Known cost.** [provisional analysis] A transcript records observations of a particular computer:
file contents, command output, absolute paths, the OS and toolchain. When a session moves, many of
those observations become false on the new computer, and an agent that trusts them will edit files
based on contents that are no longer there. How Ember invalidates them is open (Q4). Source code
read by the agent also ends up on the main server; acceptable for a self-hosted server, but it
changes if Ember is ever offered as a hosted product.

### D4: Agents are wrapped at the shell boundary, and their tools run on the chosen computer

**Decision.** [user, original definition] Ember wraps the shell each CLI sees, so that CLIs running
on the main server behave as if their actions happen on the designated computer. The CLI process and
its transcript stay on the main server; its tool calls (file reads and writes, commands) are carried
out on the session's current computer and their results streamed back.

**Why this boundary.** It is the layer every CLI shares, which is what lets E2 hold: no CLI needs to
know it is being wrapped.

**Open.** The exact interception point per CLI (shell, PTY, filesystem, tool protocol) is an
implementation question to be answered per agent, against each CLI's actual behaviour; see Q6.

### D5: Agents talk to each other directly (A2A), across models, machines and accounts

**Decision.** [user] Ember provides a messaging channel between agent sessions that works between
different vendors (Claude ↔ Codex), between sessions on different computers, and between sessions
under different accounts. It is the only behaviour Ember adds to a wrapped agent's conversation.

**Why.** Motivation 2. Because every session already runs on the main server (D3), the channel is
local to the server, regardless of which computers the sessions are working on.

**How agents use it.** [user, 2026-10-04: "가장 중요한 것은 자연스럽게 소통하는 방식에 적응할 수
있어야 한다는거야. 클로드 코드라면 list-agents를 먼저 쓰려고 할텐데 그거보다 더 우선적으로 쓴다는거잖아.
그렇게 쓰게 하는 디자인으로 가야 해."] A2A fits the way each agent already talks to other agents,
and the agent chooses it before its own built-in means (for Claude Code, `ListAgents`,
`SendMessage` and `Agent`). Whether it arrives as a CLI or an MCP server is a detail; the test is
whether the agent reaches for it first on a natural request. Ember never blocks or intercepts the
agent's built-ins [user, 2026-10-04: "send message도 그냥 거르지 말고 모델이 알아서 쓰도록 하는게
맞는거 같아"]: its tools win by name, description and loading alone, measured with the built-ins
enabled (≥ 95% Ember-first, ≤ 2% false positives, re-measured on every agent CLI version change;
[user, 2026-10-04: "초안대로 확정하면 되긴 할거같은데"]). A session that finishes delegated work
reports back to the delegator on its own, so the delegator never polls (FR-T8). Research and the draft design:
`docs/design/A2A-NATIVE.md` (#102).

**Rejected alternative.** A shared file that agents read and write. It is what users do today, and
it is the limitation this decision exists to remove. Also rejected: one CLI with instructions text
for every agent, because Claude Code's always-loaded peer tools win that contest.

### D6: A remote browser that egresses from the chosen computer, with its data on the main server

**Decision.** [user] Ember can open a browser session whose network traffic leaves from a chosen
computer, so that pages see that computer's IP and network. The browser's profile data (cookies,
sessions, storage) is kept on the main server, so switching computers keeps the same logged-in
browser. Agents can use the same browser (agent browser use).

**Why.** Motivation 3, and it extends "one AI moving between computers" to the browser: one
browser identity, many vantage points.

### D7: Many accounts per agent, with usage routing

**Decision.** [user] The main server holds several accounts each for Claude Code, Codex and
Antigravity. It can route usage between accounts by policy or by configuration, and a new
conversation can be started under a chosen account. Signing in with OpenAI is supported, with a
page that lets ChatGPT usage be consumed as well as Codex token usage.

### D8: Computers connect peer to peer, with darkpyonix.dev relaying the hole punch

**Decision.** [user] The main server connects to each computer peer to peer, through either a
tunnel implemented in Rust or an existing mesh product (the brief named "tailcat"; after review the user
allowed **iroh** conditionally; see Q7 and SPEC `NFR-N1`). The DarkPyonix central server, `darkpyonix.dev`, relays NAT hole punching
so that, as with Paseo, users do not have to think about connectivity.

**Update (2026-10-30).** The relay part of this decision is superseded by D36: the main server is the relay and the transport moves to letify-link once it ships. `darkpyonix.dev` keeps the address directory and sign-in.

**Consequence.** This also settles the HTTPS problem `ember/proxy/` has had for phones and tablets
(`ember/proxy/docs/BACKGROUND.md` §7-1), whichever transport is chosen [provisional].

### D9: The client is native (compose-rust, the IDE on dioxus-compose); the IDE window is wrapped VS Code Web

**Decision.** [user, 2026-10-03] The client has no webview (E1). The IDE window is a webview
running VS Code Web through Ember's wrapping layer, whose current implementation is
`ember/proxy/` (merged in #1).

[user, 2026-10-26: "dioxus-compose를 쓰고 있는데 왜 위젯을 쓰고 있어? 그럴거면 compose-rust로 기본
UI 구현하고 IDE 부분만 dioxus-compose로 해야지"] The base UI (launcher, shell, 홈, 프로젝트, 세션,
conversation, 채널, 루틴, 버전 관리, 에이전트, 파일, the computer monitor, characters, settings and
search) is built on `compose-rust` directly. Only the IDE part is built on `dioxus-compose`: the
Ember 에디터 screen and the workbench chrome drawn with Code-OSS CSS (D9a, D23). Both run in one
app, with no webview.

**Previous wording.** Until 2026-10-26 this decision read "The launcher and the conversation
screens are built on `dioxus-compose`", and the client was written that way: about 21k lines of
`ember/app/src/ui/` use dioxus-compose's typed Compose widgets (`Column`, `Row`, `Button`,
`Card`, ...) with Dioxus hooks and signals, and almost no HTML or CSS.

**Why it changed.** dioxus-compose exists for the editor core (D9a): it lays out HTML and CSS so
that the Code-OSS workbench can be drawn with Code-OSS's own CSS. A screen that only places
Compose widgets does not need that layer; it adds a second runtime (a Dioxus `VirtualDom`) and a
second copy of every widget change (a feature reaches the base UI only after compose-rust and then
dioxus-compose ship it). The owner's point is that a widget screen should be written against the
toolkit that owns the widgets.

**Status.** Not migrated. The plan (API mapping, gaps, how the two coexist in one window, order)
is #220; until it lands the base UI stays on dioxus-compose's widget path.

**Tauri.** [user, 2026-10-03] The Tauri scaffold has been deleted ("Tauri 스캐폴드가 왜 필요해?
지워."). The Tauri parts of `docs/design/INTEGRATION.md` are marked obsolete; its VS Code runtime
choice (D10) and mobile WebView notes still apply to the IDE window.

### D9a: "Open IDE" has three different implementations; Ember's own is the editor core

**Decision.** [user, 2026-10-03: "vscode 옵션은 ide를 웹으로 띄우는거고, gateway는 컴퓨터에 깔려있는
jetbrains gateway에 명령을 내려서 띄우는거고, 엠버 자체는 에디터 코어로 구현해야지? … 그게 바로
dioxus-compose를 구현하고 있는 이유일텐데?"]
- **VS Code**: VS Code Web on the session's computer, wrapped by `ember/proxy/` (D11).
- **Gateway**: a command to the JetBrains Gateway installed on that computer.
- **Ember**: our editor core, drawn by `dioxus-compose`: no webview and no JavaScript engine.
  dioxus-compose rebuilds the Code-OSS workbench DOM in Rust and renders it with Code-OSS's CSS. A
  native code editor widget replaces Monaco. Ember owns the connection layer: the Code-OSS server's
  management and extension-host connections, the remote filesystem, and the per-category bridges
  (`docs/design/EDITOR-CONNECTION.md`).

**Consequence.** M8 (the editor core) is in scope and dated (`PROJECT.md`). The official Node
Extension Host is still never reimplemented (E4): the editor core speaks Code-OSS's own
renderer ↔ extension-host protocol to it. That protocol is internal, so each OSE build pins one
Code-OSS commit, and the actor table is generated from that commit.

### D10: Two VS Code runtimes: OSE by default, the official build as an option

**Decision.** [user, 2026-10-03: "이거 기본값은 OSE 빌드본으로 하고, 유저가 세팅에서 바꿀 수 있도록
하는거 아니었나?"; `docs/design/INTEGRATION.md`] The IDE window supports two VS Code runtimes:

| | OSE | VSC |
| - | --- | --- |
| Build | Compiled by DarkPyonix from the MIT source | The user's own installation of Microsoft's build |
| Marketplace | Open VSX | Microsoft Marketplace |
| Default | Yes | No |
| Licence | MIT | Microsoft's licence, the user's responsibility |

**Consequence for E4.** The 09-22 rule "the official marketplace must work" became "the marketplace
matching the chosen runtime works": the OSE default cannot reach the Microsoft Marketplace.

**Licence of Microsoft's VS Code Server.** [darkpyonix leader's reading, 2026-10-03, not legal
advice] Its licence (code.visualstudio.com/license/server) allows using it "with Microsoft Visual
Studio Code to develop and test your applications". It forbids hosting, sharing, publishing,
renting or leasing it, and forbids combining it "with any of your applications for others to use".
So:
- OSE is the default.
- VSC is offered only as the user's own `code serve-web` from their own Microsoft install, labelled
  as the user's responsibility.
- Ember never downloads, bundles or installs Microsoft's server.
- OSE uses Open VSX only; the Microsoft Marketplace's terms are for Microsoft products.

**Mobile without Node.** [user] On Android and iOS the IDE window must work without Node, through a
`serve-web`-compatible Rust backend or by direct local access through web APIs. This does **not**
reimplement the Extension Host (E4): without Node there is no Node extension host at all, so only
extensions that ship a web build (VS Code's browser web-worker extension host, as on `vscode.dev`)
run there. Extensions that need Node require a VS Code server on a computer.

### D11: The 09-22 VS Code decisions still hold, inside the IDE window

These were decided for the IDE window and are unchanged in substance. Their full reasoning is in the
09-22 version of this document (git history) and in `IMPLEMENTATION.md`:

- **Wrap, don't fork.** Behaviour changes to VS Code Web come from CSS/DOM injection and a bridge on
  top of an unmodified build. `ember/proxy/` is that layer.
- **Tab detach is emulated** by detecting the gesture in the webview and opening a new window with
  the file's state, not ported from Electron.
- **The webview ↔ native bridge** uses each platform's own message-handler API
  (`WKScriptMessageHandler`, WebView2 `postMessage`) behind one Rust trait.
- **No Monaco replacement before the product ships.** A Compose-native editor core is the long-term
  M-series ambition, gated behind a working product; it starts, if ever, with overlay-style
  extensions (diagnostics, CodeLens, hover, inline completions), never with webview panels.

### D12: DarkPyonix is a service Ember talks to, not a dependency it vendors

**Decision.** Unchanged from 09-22. The DarkPyonix kernel is its own process with its own contract,
now defined in `darkpyonix-core` (`docs/PROTOCOL.md`, `docs/api/*`). Ember depends on that
contract, not on the kernel's internals.

### D13: `ember/proxy/`'s hub is transitional; its agent adapters are kept and moved to the main server

**Decision.** [provisional] `ember/proxy/dpx/hub/` puts several computers on one home screen by having
each computer report the transcripts it holds locally (`~/.claude`, `~/.codex`). E3 replaces that
model: transcripts live on the main server, not on the computers. The hub is marked transitional.
The adapters in `ember/proxy/dpx/agents/`, which parse Claude Code and Codex transcripts, stay useful
unchanged, because the main server is now where those transcripts are; they are kept and
relocated, not removed.

### D14: Antigravity runs without its own prompts; Ember's reinforced hook is the only gate

**Decision.** [user, 2026-10-03: the "reinforced hook" design, relayed by the darkpyonix leader]
agy's print mode cannot ask for a permission: it soft-denies every non-read-only tool, and a
`PreToolUse` hook answering `allow` does not lift that (agy 1.2.10, recorded). So Ember runs
`agy -p … --output-format stream-json --dangerously-skip-permissions` (AionUI does the same) and
makes its own `PreToolUse` hook (matcher `*`) the only gate:

1. The hook config sets an explicit `timeout`, and Ember answers `deny` before it expires. agy
   drops a hook that answers too late and **runs the tool** (AionCore measured ~30 s on 1.1.9 and
   denies at 20 s). If the timeout is ignored or capped, Ember denies at 20–25 s and offers a retry.
2. The hook process prints a valid `deny` and exits 0 on any error (missing environment, parse
   failure, non-2xx, unreachable server, unknown session).
3. The callback is authenticated by a fresh random token per session, re-issued when the session is
   recreated and revoked when it ends. Ending or cancelling a turn denies every pending approval.
4. Before every run `agy -p /hooks --output-format json` must list Ember's hook, else the run is
   refused (hooks.json has been silently dropped in the past).
5. `--sandbox` is used as well (reversed 2026-10-25, see #65 below), and the agy version is pinned. Another version is warned about and
   runs read-only until the tests below pass on it.
6. A global hook, if used, passes through when Ember's environment is absent, so the user's own agy
   sessions are unaffected.

Before merge, these must pass on the pinned version and be recorded in SPEC §A (run only in
`.scratch/` with harmless commands): the hook runs under skip-permissions; `deny` blocks; a long
`timeout` holds the tool; hook failure, crash, garbage and missing binary do not run the tool;
sub-agents, `call_mcp_tool` and browser tools are hooked; `deny` wins among several hooks; parallel
tool calls are each gated. If any fails, Antigravity sessions ship read-only.

**Outcome on agy 1.2.16** (2026-10-03, SPEC §A "Antigravity"): all pass, browser tools untestable
(none offered without a connected browser). Pinned to 1.2.16; other versions run read-only. Unlike
AionCore's 1.1.9 measurement, a hook past its timeout is killed and the tool is **not** run, and the
explicit `timeout` (3600 s) is honoured, so Ember waits for the user up to 3480 s instead of 20 s.
Two fail-open answers were found and are avoided by the hook itself: exit 0 with an empty stdout,
and `ask` (skip-permissions approves it). The hook stays in the per-session `--add-dir` folder, so no
global config is written and condition 6 does not arise.

Rejected: Antigravity sessions read-only until agy can ask headlessly (loses the agent); hooks in
the project's `.agents/` (writes into the user's repository).

**Status.** Implemented (#64, `ember/server/src/agents/antigravity.rs`). Open: shell writes fail
under `--sandbox` even inside the project (#65).

**Owner decision (E11, #65, [user, 2026-10-25: "K1/K2 -- 우리 웹 소켓으로 하나?, K5 -- 실제 구현까지 보류 | 외 전부 추천대로"]).** This reverses the 2026-10-03 decision to keep `--sandbox`. The default becomes `EMBER_AGY_SANDBOX=0` (no `--sandbox`) and Ember's hook is the gate, because the sandbox denies shell writes in the working directory. `EMBER_AGY_SANDBOX=1` turns it back on. The code change is tracked in #65.
### D15: REST paths carry no version

**Decision.** [user, 2026-10-03: "REST API의 버저닝은 죄악이야"; a principle for every REST API,
confirmed for Ember's own through the darkpyonix leader] Ember's REST API moves from
`/api/v1/...` to `/api/...`, in one change across server, client, app, proxy and docs after the
root regrouping (#60). The API changes only by addition. The push WebSocket's `v` field goes too
[user, 2026-10-03]: messages only ever gain fields, a receiver ignores fields it does not know,
and there is no version negotiation. The remote browser's stream protocol drops its `v` by the
same principle [user principle, applied 2026-10-04 through the darkpyonix leader]. The same holds
for Ember's other own protocols: the node daemon's `/v1` prefix and `protocol` number, the
transport service names (`ember-server/1` is now `ember-server`) and the IDE bridge's `version`
field. A file format keeps its version (the transcript export's `format`/`version`). The hub's
paths follow `darkpyonix-core`, whose `/v1` prefix is removed (core #42, merged).

Rejected: keeping `/api/v1` alongside `/api` (two names for one API).

**Status.** Implemented (#63, #85): routes, push messages, the browser stream, the node daemon
and the IDE bridge carry no version, with no alias for the old paths. The hub side waits for the
hub address decision (darkpyonix #41; #62, draft PR #75).

### D16: A fork uses the agent's own fork, and copies the conversation only as the fallback

**Decision.** [user, 2026-10-25: "에이전트의 자체 포크를 사용하고 fallback으로 대화 복사를 해"]
Forking a session (FR-S5, #78) asks the wrapped agent to fork its own native session when it can
(Codex at a chosen turn; Claude Code and ACP agents from the last turn). When it cannot, Ember
copies the stored conversation into a fresh agent session. The earlier Ember-level fork (#114,
#123) stays as that fallback. Evidence per agent: `docs/design/NATIVE-FORK.md`.

Rejected: copy only (loses the agent's own context: tool results, reasoning, caches); native
only (Antigravity has no fork, so the action would be missing for it).

**Status.** Written, not yet compiled.

### D17: Agents outlive the server: a shim per session holds the agent, the server reattaches

**Decision.** [user, 2026-10-25: "일단 ember가 그러면 죽지 않도록 데몬이 될 필요가 있을거 같은데?
에이전트들이 동작하다가 메모리 오버나는 일이 한둘이 아니라서 ember가 죽어버리면 다죽는 상황은 없어야
할거같아. 그리고 그러면 ember를 업데이트 하기 위해서 모든 세션을 다 꺼야 하는데 그게 맞아? herdr은
herdr 업데이트 해도 세션 이전이 가능하던데 그렇게 할 방법을 찾아봐."] Each session runs its agent under
a small shim process (the containerd-shim pattern) that holds the agent's stdio. The ember server
reattaches to the shims after a crash, a restart or an update. An update hands live sessions to the
new server without stopping them (herdr's `herdr update --handoff` is the model). If a shim dies
too, the session comes back through the agent's native resume. Each session has its own memory
limit, so one agent running out of memory ends only its own session.

**Why.** Agents run out of memory often. When the server owns them, one such failure ends every
session, and every update needs every session stopped.

Rejected: the current coupling, where the agent is a child of the server with `kill_on_drop`, so a
server exit kills every agent; stopping all sessions to update the server.

**Owner decision (E9, [user, 2026-10-25: "K1/K2 -- 우리 웹 소켓으로 하나?, K5 -- 실제 구현까지 보류 | 외 전부 추천대로"]).** The memory limit is a 1 s RSS poll by the shim, not `RLIMIT_AS` (V8 reserves a large address space and would die at the limit); the interval is configurable. After the agent exits the shim waits up to 1 hour for the final ack (E10 in the owner's list), configurable.

**Status.** Built in part (#128); see FR-S8.

### D18: A team member is a harness plus an account, or an account-switching pipeline

> **Partly superseded by D30 (2026-10-25).** The account, pipeline and proxy choice moves from
> the member to a profile bound to the harness. The harness choice per member and the owner
> decisions below on the proxy env still hold.

**Decision.** [user, 2026-10-25: "팀원 하나를 만들 때, 지정해야 할 것이 하네스가 뭔지, 어떤 계정으로
실행할건지 -- 혹은 계정 자동 전환 파이프라인으로 할건지 그 두가지를 지정해서 실행 가능하게 해야 해.
그리고 opencodex와 같이 프록시도 지원해서 클로드 코드에서 API 모델이나 codex 모델도 지원을 할 수
있어야겠지"] A team member is defined by two choices: the harness (Claude Code, Codex, ...) and either
one fixed account or an automatic account-switching pipeline (D7's usage routing). Optionally a model
proxy path sits on top, as opencodex does: for example Claude Code with `ANTHROPIC_BASE_URL` pointed
at a proxy, so it runs an API model or a Codex model.

**Why.** The harness and the model source vary independently. A member should be creatable with
exactly those choices.

Rejected: tying a harness to its vendor's models only; choosing the account only at session start
by hand.

**Owner decisions (E1, E2, [user, 2026-10-25: "K1/K2 -- 우리 웹 소켓으로 하나?, K5 -- 실제 구현까지 보류 | 외 전부 추천대로"]).** `team spawn` with neither an account nor a pipeline keeps defaulting to the pipeline (the account router); the strict exactly-one check stays in the member API. The proxy env is added to Codex and agy one harness at a time, each only after an integration test verifies the CLI's real proxy or base-URL setting; until then only Claude Code gets it.

**Status.** Member definition and the Claude Code proxy env are built (#129, awaiting CI). The proxy server is not part of Ember. Verification issues for Codex and agy are filed.

### D19: Agent API traffic exits through the current computer

**Decision.** [user, confirmed 2026-10-25 through the leader session; the owner's words: "실제로
네트워크 통신 마저도 각 컴퓨터로 보내는 것이기 때문에 클로드 서버에서는 각각의 컴퓨터에서 클로드
코드가 동작하도록 IP가 잡혀야 한다는거야."] Not only tool execution (D4) but the agent CLI's own API
traffic leaves through the session's current computer, so the model provider sees that computer's IP.
Candidate: the CLI gets `HTTPS_PROXY` pointing at a proxy that forwards through ember node.
The owner confirmed this reading (traffic, not only tools) on 2026-10-25.

**Owner decisions (E3, E4, [user, 2026-10-25: "K1/K2 -- 우리 웹 소켓으로 하나?, K5 -- 실제 구현까지 보류 | 외 전부 추천대로"]).** CONNECT only: no plain `http://` proxying (`HTTP_PROXY` is not served). A tunnel already open stays on its computer after a switch and the proxy stops only when the session ends (an idle release is not an end); a long stream can therefore keep leaving from the old IP until it closes.

**Why.** To the provider, Claude Code should look like it runs on that computer. D6 already does
this for the remote browser.

Rejected: letting API traffic leave from the main server's IP.

**Status.** Built, unverified against real CLIs and computers (#130): a per-session loopback CONNECT proxy tunnels over the node's existing `/egress`. A tunnel already open keeps its computer after a switch; new connections follow the new one.

### D20: A session knows its computer; a lost computer falls back to the main server

**Decision.** [user, 2026-10-25: "모델이 본인이 실행되는 컴퓨터가 뭔지를 알고 기본적으로는 메인
서버에서 실행되지만 available 한 컴퓨터 리스트가 있고, 그 중에서 유저가 일단 활동하라고 명시적으로
지정한 컴퓨터가 있는거고, 유저가 컴퓨터를 잠시 종료하거나 해서 갑자기 그 컴퓨터에 접근이 안되는
경우 자동으로 메인 서버에서 작업을 하다가 다시 그 컴퓨터가 감지되면 다시 복귀해서 작업할 수도 있어야
해. 컴퓨터 연결 상태 자체가 모델에게 알림으로 가야 한다는거지. 무효라고 알려주기보다는 명시적으로
컴퓨터를 자기가 이동해서 쓰고 있다고 인지시키는게 좋겠지?"] The default location is the main server.
There is a list of available computers, and the user designates one to work on. When the designated
computer becomes unreachable, the session falls back to the main server automatically, and returns
when the computer is detected again. Every connection state change is told to the agent as "now
working on X (moved)", never as "invalid".

**Why.** The agent should know where it is, and a lost computer should not stop work. Telling it
that it moved is clearer than telling it that its observations are invalid.

Rejected: failing tool actions while the computer is away; wording the notice as invalidation.

**Owner decisions (E5, E6, [user, 2026-10-25: "K1/K2 -- 우리 웹 소켓으로 하나?, K5 -- 실제 구현까지 보류 | 외 전부 추천대로"]).** E5: the session response exposes the designated computer and the current computer to clients as read-only fields (approved as recommended; the code is in another PR). E6: the loss detection default stays 2 probes x 10 s (about 10-20 s) and is configurable; it is revisited after the two-machine disconnect test.

This closes Q2 (who moves a session): the user designates, and the server moves it on a connection
change. An agent-triggered `switch_computer` is not part of this decision.

**Status.** Not built; today only the user switches (#34).

### D21: A low-level terminal view of other sessions, apart from team messages

**Decision.** [user, 2026-10-25: "한 에이전트가 다른 에이전트의 터미널에서의 동작을 바로 보는게
안되는거같긴 하거든? 그러면 가끔 대답을 안하는 에이전트가 발생하는 경우 소통이 잘 안되는 문제가
있는거 같아. ... 니가 추가로 herdr로 지금 다른 세션이 뭐 하는지 볼 수가 있잖아. 그런 로우레벨 기능도
있으면 좋을거같아."] Apart from team messages (D5), an agent with permission can read another
session's live and past output and send it input or an interrupt, as A2A tools. The model is herdr's
`pane read`, `pane send-text` and `pane send-keys`.

**Why.** An agent that does not answer cannot be reached by messages. Reading its terminal shows
what it is doing.

Rejected: messages as the only channel between sessions.

**Owner decisions (E7, E8, [user, 2026-10-25: "K1/K2 -- 우리 웹 소켓으로 하나?, K5 -- 실제 구현까지 보류 | 외 전부 추천대로"]).** The implicit grant runs only from a team leader to its own teammates; a teammate acts on the leader's terminal only with an explicit user grant. The `ember-a2a` CLI subcommands and the agent instruction text are a separate follow-up issue, so #132 closes at the tools.

**Status.** Server tools built (#132, FR-T9); the CLI subcommands are not.

### D22: The client has navigation tabs: Home, Project, Channels

**Decision.** [user, 2026-10-25: "Ember IDE의 네비게이션 탭 구성을 홈(일반적인 채팅창+토큰 소모량+구독 사용량 남은거+하네스 목록), 프로젝트(기본 화면), 채널(프로젝트 내 에이전트들이 사용자에게 요청하거나 보고하는 내용의 종합+@로 에이전트 태깅해서 슬랙처럼 메시지 바로 보내기 가능+몇몇의 에이전트만 뽑아서 새로운 채널 생성 가능+외부 계정을 채널에 초대 가능, 에이전트 태깅 없이는 사람들사이 대화로 취급) 등으로 하려는데 이에 따라 스펙 추가해줄래?"] The client's top navigation is a set of tabs. "등으로" means the list may grow, so the tab set is open-ended.

- **Home**: a general chat window, token consumption, the remaining subscription usage, and the list of harnesses (agents).
- **Project**: the default screen, which is today's project and session view (`FR-L1`–`FR-L9`).
- **Channels**: per project. An aggregate feed of what the project's agents request from or report to the user. The user can mention an agent with `@` and message it directly, like Slack. A new channel can be created with a chosen subset of the project's agents. External accounts (other people) can be invited into a channel. A message without an agent mention is human-to-human conversation and is not delivered to any agent.

**How it relates to earlier decisions.**

- D9: the tabs are native `compose-rust` screens (dioxus-compose until #220 lands), with no webview (`NFR-L2`).
- D7: Home shows usage already recorded for accounts (`FR-U3`, `FR-U4`); it adds a view, not a new meter.
- D5: a channel mention reaches an agent through the same A2A delivery as a composer mention (`FR-T6`, `FR-T3`). The agents' requests and reports in the feed reuse the idle and completion reports (`FR-T8`). The new rule is the mention gate: no mention, no delivery to agents.
- Teams (`FR-T7`, D18): a channel's agent subset is a selection of sessions; it does not create a team, and the team messaging rule still applies to team members.

**Why.** Agentic work is mostly watching and steering. A project with several agents needs one place where their requests and reports arrive, and one place to talk to a person without waking an agent. The mention gate keeps people talking to people from costing tokens or steering an agent by accident.

**Owner's answers, 2026-10-25.**

- Tokens on Home: "토큰 사용량은 진짜로 token 사용량을 말하는거야. 해당 스펙이 usage가 아니라 진짜로 소모한 토큰 량이 맞으면 괜찮고 아니면 새로 스펙 추가해." `FR-U3` and `FR-U4` did not define consumed token counts, so `FR-U7` is new; Home shows both (`FR-L11`).
- Mentions by invited people: "그래 좋아. 근데 사람별로 앞으로 몇시간 동안 자동 허용 이런 기능이 필요할거 같아." The default stays (an invited person's mention reaches no agent unless the owner allows it, `FR-T15`); a per-person time-boxed auto-allow is added (`FR-T17`).
- Q10: "그 채널만 볼 수 있어. 디스코드나 슬랙 생각하는게 편할거야." (`FR-T14`)
- Q11: "분류되지 않은 프로젝트로 할당하도록 해. 나중에 프로젝트로 옮길 수도 있겠지." (`FR-L13`)
- Q12: "음 메인 서버에 사람 전용 대화 jsonl 파일이 필요하려나" (tentative wording; `FR-T16`)
- Q13: "탭은 기능으로 올라올 필요가 있으면 더 올라오면 되는거야. 채널 하나가 여러 프로젝트에 걸칠 수 없어. 한 채널은 한 프로젝트에 속한거야. 때문에 채널 페이지에 들어가면 프로젝트별로 분류된 섹션의 하위에 채널들이 하나씩 놓여있는 형태인거지. 한 프로젝트는 기본적으로 하나의 채널을 가지고, 추가 채널을 생성할 수 있는 형태인거야." (`FR-L10`, `FR-L12`, `FR-T10`)

**Channel details, 2026-11-06 [provisional, relayed by the supervisor, not user-confirmed].**

- Mention syntax: in a channel a mention is `@<agent>`. The session composer keeps `@@` (`FR-T6`). Rejected: one syntax for both, because a channel is Slack style and a single `@` is what people type there (`FR-T12`).
- Agent reply: an agent replies into a channel through a dedicated tool named `channel_post` (`FR-T12`). Rejected: scraping the agent's transcript for its answer, because it ties the channel to each agent's session format (rule 4).
- Storage: SQLite stays the read index and the message store. Human messages are also appended to the per-channel JSONL file (`FR-T16`) in parallel. Rejected: JSONL as the only store (no index for `after=` and unread counts) and SQLite as the only store (`FR-T16` asks for the file).
- Allowance: whole hours, capped at 168 h (`FR-T17`). Rejected: free-form minutes and open-ended allowances, because an allowance spends the owner's tokens.
- Guest: an invited person sees the message log only and has no right to answer approvals (`FR-T14`). Rejected: guests answering an agent's approval request, because that steers the owner's session.
- Invite authentication: decided below ("Invite authentication, 2026-11-06 [user]"). It replaces the earlier note that the supervisor only recommended (a).
- Confirmation: the supervisor and the leader approved the five remaining decisions of this block (mention syntax, `channel_post`, storage, the 168 h cap, guest sees the log only). The supervisor message did not name them as owner-confirmed, so they stay [provisional] until the owner confirms them in their own words.

**Invite authentication, 2026-11-06 [user, relayed by the supervisor].** The owner chose option (a): "(a) 채널 한정 초대 링크" (a channel-scoped invite link). The supervisor relayed only this choice, not the owner's own sentence, so the quote is the option's label. This closes the remainder of Q10. The model (`FR-T14`, `FR-T15`, `FR-T17`):

1. The channel owner creates an invite. It is a single-use, channel-scoped link carrying a random token. The server stores only a hash of the token, never the token.
2. Accepting the link yields a per-person Bearer credential scoped to that one channel. The server stores only its hash.
3. Revoking the invite, or removing the person, makes the credential invalid at once (the next request gets 401).
4. A guest credential is refused on every route except that channel's message log: reading it and the guest's own posts there (a guest's mention is what `FR-T15` and `FR-T17` gate, so a guest can post). It can never answer approvals, see the feed or other channels, or reach projects, sessions, files or computers.
5. An allowance (`FR-T17`) is in whole hours, capped at 168. By default an invited person's mention reaches no agent unless the owner allows it (`FR-T15`).

**Rejected** (the supervisor's relayed choice gave no reasons; recorded as the alternatives the choice excluded, reasons not stated): other mechanisms than a channel-scoped invite link. The single-use, hash-only storage and the immediate revocation are the supervisor's description of (a); where this text adds a detail (posting by guests), it is [provisional].

**Interaction with D37.** The server token middleware (`auth.rs`) must treat a guest credential as a different credential class: valid only on the channel guest routes, never accepted where the device key or the local token is. A guest credential must not pass as the server token on any route.


**Open items on invites (questions, not decided).**

- Q-a: How long does an unused invite live before it expires (a default and a maximum)?
- Q-b: Does a guest have a display name the owner sets at invite time, or one the guest types on acceptance? `FR-T15` attributes each message to the account that wrote it.
- Q-c: May a guest post at all? The text above assumes yes, because `FR-T15` speaks of an invited person's mention; the supervisor said only "log only". Confirm.
- Q-d: Does a guest credential expire by itself, or live until revoked?
- Q-e: Can a guest receive pushes (the `channel_message` WebSocket event) or only poll the log? Push needs the WebSocket route to know the credential class (D37 part 3, Origin and Host checks apply to guests too).
- Q-f: Reachability: a guest outside the owner's network cannot reach a loopback or LAN server. Does the invite link need a relay or the device layer (`FR-N`), and does a guest become a device?
- Q-g: Does the owner see a list of invites and guests (pending, used, revoked) in the channel?

**Later (2026-10-25).** D26 fills the open tab set with eight entries; D28 renames Unsorted to 기본 프로젝트.

**Status.** Specified only (`FR-L10`–`FR-L13`, `FR-T10`–`FR-T17`, `FR-U7`). Not built. Still open: the exact JSONL layout of `FR-T16` and the invite details listed under Open questions (invite lifetime, guest identity, reach beyond the v1 server).

## Open questions

| ID | Question | Status |
| -- | -------- | ------ |
| **Q1** | How do a project's computers relate: copies of the same workspace (the same repository checked out on a Mac and a Linux box), machines with different roles (iOS builds on the Mac, GPU work on Linux), or simply "whichever computer I am at"? | Open; decides how much of a session's observations survive a move |
| **Q2** | Who moves a session: the agent (a `switch_computer` tool plus a routing policy, e.g. "needs CUDA → GPU box"), the user, or both? | **Closed** [user, 2026-10-25]: the user designates the computer and the server falls back and returns on connection changes; see D20 |
| **Q3** | When a session moves, does an open IDE window follow it, or stay with its computer? | Open |
| **Q4** | How are a transcript's computer-specific observations invalidated on a move? Proposal: split the transcript into a computer-independent part (intent, decisions, plans, conclusions) and a computer-specific part (file contents, command output, paths, environment, background jobs); record each file observation as (path, content hash, computer, time) and re-hash on the new computer so only changed files are flagged; keep the environment description in one replaceable block instead of appending; scope "read before edit" to a computer. | [provisional] proposal. v0 (a system notice on a switch) is implemented (#34, SPEC `FR-S7`); the hash comparison is written from the tool-call log (SPEC `FR-S7`, #79); a separate recorded (path, hash, computer, time) table was not built, since the log already names the files |
| **Q5** | A job started on computer A when the session moves to B: kill it, keep it and notify on completion, or block the move? Proposal: keep it running and notify. | [provisional]; built as proposed in ember node (`FR-X4`, `ember/node/src/jobs.rs`) |
| **Q6** | For each wrapped CLI, where exactly is the interception point (shell, PTY, filesystem or tool protocol) that keeps its native behaviour intact? Candidates per agent: the vendor's own headless protocol, or the Agent Client Protocol (ACP), which OMP exposes natively and Claude Code / Codex / Gemini reach through adapters. | **Decided per agent:** Antigravity [user] (D14, implemented #64); Claude Code and Codex [provisional], implemented (#14); OMP and other ACP agents [provisional], implemented (#61), real OMP not run (#53); Gemini open. Details: **Claude Code:** `--print` stream-json with `--permission-prompt-tool stdio` (#14). **Codex:** `codex app-server` directly (#14); the maintained ACP adapter (`agentclientprotocol/codex-acp`) is itself a translation layer over app-server, so it can only lose detail (approval choices, turn steering, thread ids, usage), and the older one compiles Codex internals pinned to an old release, breaking FR-A1. **Antigravity**, decided in D14 [user]: `agy -p … --output-format stream-json`, one process per turn, `--conversation=<id>` for native resume (agy 1.2.10, recorded in `ember/server/tests/fixtures/agy/`). Print mode cannot ask for a permission: it soft-denies the tool, and a `PreToolUse` hook answering `allow` (even with `permissionOverrides`) did not lift that (recorded). So Ember runs agy with `--dangerously-skip-permissions` and makes its own `PreToolUse` hook the only gate: the hook (`curl` to a local route holding a per-run secret) blocks until the user answers, `deny` hard-blocks, an unreachable server denies, and before every turn `agy -p /hooks --output-format json` must list the hook or the turn is refused (D14; tested on 1.2.16 in SPEC §A). The hook, a rules file (instructions) and `mcp_config.json` live in a per-session folder passed with `--add-dir`, agy's own customization root (`.agents/`); nothing in agy or in the user's `~/.gemini` or project is changed. Costs: the folder shows up to the model as an extra, first-listed workspace (the rules file tells it to stay out), the stream carried no tool output on 1.2.10 (1.2.16 has it), and agy has no config-dir variable, so Antigravity accounts (D7) are not possible yet. Rejected: hooks in `~/.gemini/config/hooks.json` (shared by every agy run and cannot hold per-session MCP servers), hooks in the project's `.agents/` (writes into the user's repository), `--input-format stream-json` (one process per session, but its input schema is undocumented). **OMP and other ACP agents** *[provisional]*: the agent's own ACP server over stdio, through one generic ACP client (#53, ACP protocol v1); OMP's is `omp acp` (oh-my-pi v18.5.0, read from source, not yet run). Ember is the ACP client, so file reads/writes and shell commands that the agent routes through `fs/*` and `terminal/*` are executed by Ember itself, which is the interception point: they run on the session's computer through its node, with no shim. Tools the agent does not route through ACP (search, LSP, …) still read the main server's filesystem, which the project mount covers. Rejected: OMP's own `--mode rpc` NDJSON protocol (OMP-specific; ACP serves every ACP agent with one adapter). Claude Code / Codex through ACP adapters stays rejected as above. Others (Gemini): open. |
| **Q7** | Which transport: iroh, rustunnel, a tunnel written in Rust from scratch, or tailcat? | **Decided, conditionally** [user, 2026-10-03]: **iroh 1.0**, behind a replaceable interface (SPEC `FR-N5`); if it misses `NFR-N1`, our own implementation is evaluated. Implemented in `ember/transport` (#23); `NFR-N1` not yet measured on real networks (#10). tailcat was withdrawn after the user's objection (two apps on mobile). An own implementation is weeks rather than months for the basic path, since a failed hole punch falls back to the relay; it stays the replacement option. `rustunnel` is not a transport: it is a server-relayed tunnel with no hole punching, and AGPL-3.0, so it must not be linked into clients; it is only a reference for the hub's public HTTPS edge. Source: darkpyonix leader, core PROJECT Q1 (3ea7fb7). **Update [user, 2026-10-30]:** the move to letify-link with the main server as relay is D36 (`FR-N5`); iroh stays until it ships. |
| **Q8** | Is the Tauri scaffold kept long-term, and for what? | **Closed: deleted** [user, 2026-10-03] |
| **Q9** | `ember/proxy/`'s open items carry over: HTTPS for phones (likely resolved by D8), login being a thin shell, and the pre-distribution security holes in `ember/proxy/docs/BACKGROUND.md` §7-3. | Open; HTTPS over P2P is #10 |
| **Q10** | How does an external account (another person) authenticate to a channel, and what may it see: only that channel, or also the project's sessions, files and computers? | **Decided** [user, 2026-10-25]: "그 채널만 볼 수 있어. 디스코드나 슬랙 생각하는게 편할거야." An invited person sees only the channels they are invited to (`FR-T14`). **Remainder closed** [user, 2026-11-06, relayed by the supervisor: "(a) 채널 한정 초대 링크"]: a single-use, channel-scoped invite link, token hashed on the server, accepting it yields a per-person channel-scoped Bearer credential (D22). Open details: invite lifetime, guest identity, guest posting (listed after D22's block) |
| **Q11** | Is Home's general chat a session without a project (so it needs a computer, an account and a place for its transcript), or something else? | **Closed** [user, 2026-10-25]: "분류되지 않은 프로젝트로 할당하도록 해. 나중에 프로젝트로 옮길 수도 있겠지." Home chat sessions live in a built-in Unsorted project (`FR-L13`), renamed 기본 프로젝트 by D28 |
| **Q12** | Do channels persist across machines: stored on the main server and shared by every client and computer, or per client? Where do invited people's messages live when the main server is self-hosted? | **Closed** [user, 2026-10-25]: "음 메인 서버에 사람 전용 대화 jsonl 파일이 필요하려나" Per-channel append-only JSONL on the main server (`FR-T16`); the exact layout is [provisional] |
| **Q13** | Which tabs follow Home, Project and Channels ("등으로")? And is a channel per project only, or can one span projects? | **Closed** [user, 2026-10-25]: "탭은 기능으로 올라올 필요가 있으면 더 올라오면 되는거야. 채널 하나가 여러 프로젝트에 걸칠 수 없어. ..." Full quote in D22. A channel belongs to one project; every project has a default channel (`FR-L12`, `FR-T10`) |

### D23: Ember vendors Code-OSS CSS by script and builds the workbench chrome rsx

**Decision.** [provisional, leader decision 2026-10-25, user confirmation pending] *(Scope, corrected 2026-11-06: this decides how the chrome is drawn, not how it looks; the design is D25 and the mockup, see D38.)* Ember carries the Code-OSS CSS the workbench chrome needs, in `ember/app/assets/code-oss/`. `ember/ose/vendor-workbench-css.sh` fetches it from the Code-OSS commit the OSE build pins (`PINNED_COMMIT` in `ember/editor-conn/src/lib.rs`, tag in `ember/ose/VERSION`) and records the commit hash and the MIT notice in every file. Ember's only edit is `workbench-has.patch`, which turns `:has()` rules into state classes (#142). Ember builds the workbench chrome rsx (tree rows, view filter, title bar command center, action widget) and sets those classes (#143). Hover and chat markdown are drawn with `dioxus-compose-markdown` Compose widgets; there is no HTML `.rendered-markdown` path, so `renderedMarkdown.css` is not vendored.

**Rejected.** Copying the CSS by hand (it drifts from the pinned Code-OSS and loses the commit record). A stylo patch for `:has()` (the replacement cost is small). Rendering markdown as HTML with `.rendered-markdown`.

**Why.** The renderer drops `:has()` rules, so they need classes either way. A script plus a patch keeps the copy reproducible, and CI checks it offline against a committed pristine copy.

### D24: Files move between all of a user's connected devices, like AirDrop

**Decision.** [user, 2026-10-25: "Ember 스펙에 파일 전송 기능도 추가하는걸로 하자. 파일 메뉴도 UI에 추가해줘. ... 이 기능을 통해 연결된 모든 사용자 장비에 대해서 파일 전송이 가능해지는거지. 모바일까지도. airdrop 처럼 쓸 수 있게 하는거야!"] Ember has a file transfer feature and a Files menu in the UI. Any file can go between any two of the user's connected devices, mobile included, with an AirDrop-like feel: pick a device, send, the other side accepts.

- **Transport** *[provisional]*: files travel over the existing peer-to-peer tunnel and its relay fallback (D8, `FR-N1`, `FR-N5`), as another stream on the same transport. No new network path, no new account system.
- **Devices** *[provisional]*: "the user's devices" are the computers and clients registered under the user's GitHub account on the hub (`FR-N2`). Another person's device is out of scope here.
- **Receiving** *[provisional]*: a transfer needs the receiver's consent. Received files land in a known folder and are listed with a transfer history in the Files menu.
- **Reliability** *[provisional]*: large files stream in chunks and resume after a dropped connection.
- **Mobile** *[provisional]*: the phone and tablet clients take part through the OS share sheet.

**Rejected** *[provisional]*. A separate cloud upload service (files would leave the user's own devices and need storage, quotas and a trust boundary of their own). Third-party sync tools (a second app on mobile, the objection that withdrew tailcat in D8). A new transport beside the iroh tunnel. Sending to other people's devices in this first version.

**Why.** The tunnel already connects every device of the user end to end. File transfer is the missing use of it, and it removes the habit of mailing files to oneself.

**Decisions of 2026-11-06** *[provisional, 2026-11-06 supervisor]*, issue #487:

- **Who may offer**: only between the user's own devices registered under the same GitHub account (`FR-N2`). Rejected: offering to other accounts, to guests, or to unregistered devices, because each needs a trust model this version does not have.
- **Size limit**: 4 GiB per transfer by default, configurable per device, applied to the sum of an offer and checked at the offer before any byte moves (`FR-F10`). Rejected: unlimited transfers, which let one device fill another's disk.
- **File names**: only the basename is accepted. Control characters, path separators (`/` and `\`) and a leading dot are removed or replaced; an empty result gets a generated name. A folder offer keeps relative paths, every component goes through the same cleaning, and `..` never escapes (`FR-F9`). Rejected: trusting the sender's path, which is the classic path traversal.
- **Storage**: `FR-F8` keeps the downloads folder as the target; the first version writes to a received-files folder under the device's data folder, and a user-chosen location is a follow-up. A clash never overwrites. Rejected: writing to downloads in the first version, since it needs per-OS pickers and permissions.
- **Execute bit**: removed on receive. Files are written 0644, with no setuid, setgid or sticky bit. Rejected: preserving the sender's mode, which turns a received file into something runnable.

**Architecture** *[provisional, session pZ proposal, needs supervisor confirmation]*. `FR-F6` allows file network code only inside the transport crate, and the transfer endpoints are devices (the app on a desktop or phone), not servers or computers. So the transfer service is a device-side library crate `ember/transfer` (a directory under `ember/`, no new repository root entry). It owns the offer, accept, decline and expire state machine, name cleaning, size limits, receive modes and the resume ledger, and talks to peers only through `ember-transport` streams. It is tested over the in-memory fake transport. The app's `TransferService` trait (`ember/app/src/ui/files.rs`) is implemented by it. The Ember server is not in the path and gets no `/api/transfers` route. Rejected: server-hosted transfer tables, because they put offer metadata, names and sizes on the server and contradict `FR-F6`.

**Update of 2026-11-13: the server relays to a computer** *[provisional] (leader, 2026-11-13)*, issue #773. The architecture above no longer holds as written: #725 gave the server its own receiver (`/api/transfers`), and a send to a computer other than the server goes through the server too. The server forwards each chunk from the sender to the computer's `ember-node` over the node connection it already holds for that computer (the D42 boundary: one authenticated door, revocation at the server). A chunk is held in the server's memory only while it is forwarded and is never written on the server's disk. The node receives with the shared `ember-transfer` store (per-chunk and final SHA-256, `0644`, never overwriting) under `<state_dir>/received` by default, keeps a resume ledger that survives a node restart, and checks free space before it accepts. Only computers reached over the encrypted transport are targets: a computer registered by plain HTTP is refused, because the bytes would cross the network in clear. An offer is accepted on its own only when the owner account starts it to one of the owner's own enrolled computers; any other offer waits for an answer on the node, and answering for a node from another device is later work. Phones (`FR-F7`) are out of scope here. `FR-F6` is reworded to match, and `FR-F13` to `FR-F16` specify the parts.

Rejected: the node receiving directly from the sending device (option B in `.scratch/p1S/773-options.md`), because the node admits only its servers (D39, D42) and the stream protocol cannot yet carry a real chunk; a spool on the server, because the file would then exist on a third machine; plain HTTP node targets; letting the server alone decide auto-accept for every offer, because a non-owner offer must still be answered.

**Status.** Specified only (`FR-F1`–`FR-F10`, Draft; issues #152, #487). Not built. First unit: the `ember/transfer` state machine over the fake transport. The Files menu mock is separate UI work.

### D25: One client design on every OS, after the owner's reference images; Liquid Glass on iOS only

**Decision.** [user, 2026-10-25: "운영체제별 디자인 쓰지 말고 통일 디자인으로 해."] The client draws
the same screens with the same tokens and components on macOS, Windows, Linux, Android and iOS.
The look comes from the reference images the owner supplied during the mockup rounds (ref1 to
ref10: a light dashboard, an agent chat app, a warm usage card, Claude Code desktop, an agent
timeline, a system monitor, a model picker), recorded with their roles in `DESIGN.md` §1.
[user, 2026-10-25: "모바일 뷰에서 지금 하단 앱바가 안예쁜데 floating앱바 형태로 바꿔줘. iOS에서는
liquid glass 적용하도록 하고."] The one exception is iOS, where the floating bottom bar uses Liquid
Glass; every other platform draws the same bar opaque. Tokens, the Apple-style squircle (fixed
radius, smoothing 0.6: "애플의 스퀘어클과 맞지 않는 구현이야"), motion and every screen layout are in
`DESIGN.md`.

[user, 2026-11-08: "원칙으로 절대 ember UI 구현할 때 운영체제별로 UI를 일부러 다르게 구현하는 문제는 없어야 해", "유니파이드가 맞고 iOS에서 앱바만 리쿠이드 클라스야"] The app builds its toolkit theme with `Theme::unified`, never `Theme::adaptive`. An adaptive theme lets compose-rust pick the platform design system (Liquid Glass on macOS, Fluent on Windows, a Linux one elsewhere), and every toolkit-drawn part (dialog, menu, dropdown, sheet, their scrim and shape) then changes per OS; that is how Windows came to show different modals. Code may branch on the OS only for window chrome, safe areas, key labels and file-manager names, never for how a control looks or behaves. A difference between OSes is a toolkit bug and goes to compose-rust as an issue.

[user, 2026-10-25: "앱 기본 스타일에서 안쪽 코드 작성이랑 도구창이 오히려 jetbrains island라는 최신 디자인 컨셉에 더 맞아. 그래서 그쪽 디자인이 맞고, 다만 지금 좌우 메뉴바의 경우 jetbrains 느낌으로 하는게 맞을거 같아. 흰색 버튼 배경이 보기 싫네."] The editor and terminal use the app's own style: rounded island panels, tabs, tool windows and the code area, matching the JetBrains Islands concept. Only the left and right tool-window stripes take the JetBrains form: 40 px icon-only stripes, with no white background or shadow on the selected button, just a faint selection tint. Breadcrumbs stay under the tabs. [provisional] The owner did not decide this, so it is a leader decision. Every other screen keeps the mockup design unchanged, including the session panels, the status view and version control.

**Why.** One product that a user moves between machines should look the same on all of them, and a
per-OS widget set multiplies the work of every screen.

**Rejected.** Per-OS designs. A full JetBrains New UI look with the cool grey IDE palette on every
screen, which issue #151 first asked for ("Jetbrains 제품 같은 느낌"). The owner's later rounds
steered the base by the reference images, so the base stays the warm mockup palette. The JetBrains
feel is added only on the editor and terminal screens, as the 2026-10-25 correction places it (see
the Decision). Liquid Glass on every
platform (asked for iOS only).

**Status.** Design approved in the mockup (`.scratch/design-mockup/`), not built (#154). Gaps in
dioxus-compose and compose-rust are listed in `DESIGN.md` §10.

### D26: The menu is 홈, 프로젝트, 세션, 채널, 루틴, 버전 관리, 에이전트, 파일

**Decision.** [user, 2026-10-25] The client's navigation has eight entries in this order. Each was
added by the owner's own words:
- 세션, removed earlier as a "list of member conversations" and re-added in a different form: "세션
  메뉴 탭 만드는거 허용했잖아. 단지 지금 보이는 형태가 아니라 내가 지시한 대로 하라니까?" (see D27);
- 루틴: "루틴 디자인은 기존의 참고 이미지들에서 컨셉을 가져오도록 해";
- 버전 관리: "버전 관리 메뉴를 추가해줘." (D34);
- 에이전트: "에이전트 메뉴를 하나 더 추가해서 거기에 하네스와 모델, 그리고 라우팅 등의 우리가 지원하는
  기능들을 설정할 수 있도록 세팅해주자. 홈에 있던 사용량 표시도 같이 넣어주고.";
- 파일: "파일 메뉴도 UI에 추가해줘." (file transfer, D24 in PR #153).

The sidebar carries no group label ("굳이 워크스페이스 라는 글자를 표기하고 있을 필요가 없을거 같아.").
On a phone the floating bar shows 홈, 프로젝트, 세션, 채널 and 더보기, which opens the other four
[provisional: the split is the mockup's, so the bar stays at five].

**How it relates to D22.** D22's open-ended tab set ("등으로") is filled by this list; D22's Home,
Project and Channels keep their meaning, with the Home changes in D28 and `FR-L11`.

**Why.** Each entry is a place the owner works in daily; settings for harnesses and accounts were
scattered across member forms before 에이전트 existed.

**Rejected.** The four-entry menu (홈, 프로젝트, 채널, 루틴) of the first mockups; a 세션 menu that
lists every member conversation; Home holding the detailed usage (it moves to 에이전트 → 사용량, and
Home keeps the graphs, `FR-L11`).

### D27: 세션 is being inside a project

**Decision.** [user, 2026-10-25: "세션 메뉴 탭 만드는거 허용했잖아. 단지 지금 보이는 형태가 아니라 내가
지시한 대로 하라니까?"; "세션 서브탭 안에 프로젝트에 대한 세션이 있는거니까 sesshead가 세션 탭 옆에서부터
시작하도록 수정해줘"] The 세션 tab is the state of being inside a project: it opens the last project
used, with its members in a multi, single or status view, and the header switches project. A
session side list (search, one section per project) runs the full height on the left in every
view. Further owner rules for this screen: user-arranged tabs of panes with drag and drop instead
of a pane count ("탭을 추가해서 여러 pane을 사용자가 배치할 수 있도록 해줘"); a Claude Code desktop
style right panel in the single view; no project breadcrumb or back button in the single view
("굳이 상단에 어느 프로젝트껀지를 표기할 필요가 없어보여"); IDE open and the IDE chooser at the right end;
an agent communication timeline in the status view; opening a web page on the member's computer
IP; version control as a sheet. Members are named as team members ("@@@구현, @@@리더 이렇게 팀원
형식으로 해줘야지"). Layout: `DESIGN.md` §6.3.

**Why.** A session belongs to a project (D1), so the session screen is a project's room, not a
global list.

**Rejected.** A global list of every conversation as the 세션 tab; a fixed pane count picker; the
project name above the single view.

### D28: 기본 프로젝트 replaces Unsorted, and has no leader

**Decision.** [user, 2026-10-25: "Unsorted가 아니라 기본 프로젝트가 맞지 않나. 그리고 기본 프로젝트는
팀 리더가 없어야지."] The built-in project of `FR-L13` (Q11) is named **기본 프로젝트**. It has no team
leader: no crown, no leader setting, no orchestrator lane. Home's composer has a project chip
whose default is 기본 프로젝트 ("채팅 시작할 때 프로젝트 선택 옵션이 있어야 해. 프로젝트가 뭐냐가
중요하거든."), so a Home chat can start in any project.

**Why.** "Unsorted" reads as a leftover; it is where a chat starts by default. A leader orchestrates
a team, and the default project is not a team.

**Rejected.** The name Unsorted (분류되지 않은); a leader in the default project; Home chats that can
only start in the built-in project (Q11's answer stands as the default, not as the only choice).

### D29: Team character styles, chosen per project

**Decision.** [user, 2026-10-25: "에이전트 캐릭터는 그냥 팀 셋업 할 때 고를 수 있도록 하자. 각각의
디자인의 이름은 니가 적당히 지어봐."] Members are animated characters ("사각 아바타 말고 Dots 처럼
아기자기하고 귀여운 에이전트 아이콘을 만들어두도록 해. 아이콘이라기 보다는 진짜 움직이는걸로 해줘."). A
project chooses one of six styles at setup or in its settings, and all its members use it. The
owner picked the six ("1. 지금 현재 기본 디자인 좀 더 색상하고 디자인 다양화 2. B 3. 역할 소품인데
에이전트 자체는 좀 다른 디자인으로 4. D 5. E 6. G"). The names 몽글, 말랑, 크루, 포켓, 크림, 키링, the
seeded shape and colour, and 몽글 as the default are [provisional]. A leader shows a crown badge on
every style ("리더(오케스트레이터) 역할의 에이전트는 아이콘에 표기를 따로 해줘야지"). `DESIGN.md` §7.

**Why.** The owner wants members to read as teammates, and a team chooses its own look once rather
than per member.

**Rejected.** Square letter avatars; abstract directions (geometric morph, orbs, line glyphs,
monograms: "아니 다른걸로 바꾸라는게 아니고 캐릭터를 여러개 뽑아달라고."); one style for the whole app;
a style per member.

### D30: Profiles bind accounts and routing to a harness, replacing per-member routing

**Decision.** [user, 2026-10-25: "에이전트 세팅에서 지금은 팀원 별로 따로 따로 복잡하세 라우팅을 세팅해야
하는데 그러지 말고, 계정과 라우팅을 결합해서 프로필이라는걸 만들어서 프로필이 하네스에 할당되도록 해줘."]
A **profile** is a name, a target harness, an account bundle (one fixed account, or an ordered list
of accounts switched automatically), switching rules (usage threshold, failure handling, routing
policy) and an optional model proxy. Each harness has one default profile. A member follows the
profile of its harness; it has no account, pipeline or proxy of its own. A project may override the
profile per harness [provisional: the leader's simplification, "예외가 꼭 필요할 때만"]; there is no
per-member override [provisional].

**Supersedes part of D18.** D18's "a team member is a harness plus an account, or an
account-switching pipeline", with an optional proxy per member, no longer holds as the user-facing
model. The harness choice per member stays; the account choice, the pipeline and the proxy move to
the profile. D18's owner decisions on the proxy env per harness (verified one at a time) still
apply, now to profiles. D7's many accounts and usage routing are unchanged; profiles are how they
are configured. `FR-U1`, `FR-U6` and `FR-A9` are marked accordingly; profiles are `FR-L17`.

**Why.** Setting accounts and routing for every member repeats the same choice many times; the
choice belongs to the harness.

**Rejected.** Per-member account, pipeline and proxy settings (the "팀원 구성" table); a per-member
profile override.

### D31: Permission modes are chosen per harness

**Decision.** [user, 2026-10-25: "모드도 필요해 바이패스 모드인지 자동인지 그런거. 새 채팅에는
없더라고."] Wherever a session is started (Home, the session composer, new member, routines,
project defaults), a permission-mode chip lists the modes the chosen harness supports: ask for
every command, auto-approve edits only, bypass (allow everything), plan mode. Bypass is shown in
the warning colour. Each harness has a default mode in 에이전트 → 모델. The four modes, their names
and which harness supports which are [provisional] until each CLI's real modes are checked by an
integration test (AGENTS.md rule 4); bypass is today's approve-all (`FR-A5`).

**Why.** The owner switches between careful and unattended runs often, and the choice belongs at
the start of a session, not in a menu afterwards.

**Rejected.** One global approval policy per project (the mockup's earlier "승인 정책"); a mode list
that ignores what the harness supports.

### D32: The IDE chooser offers exactly three IDEs

**Decision.** [user, 2026-10-25: "IDE 열기 기능에서 지금은 이상하게 Cursor, Zed가 추가되어 있는데
빼줘. 그리고 VS Code ssh로 연결되는거 아니야."] The session header's IDE chooser lists three entries,
as D9 and D9a already decided: **Ember 에디터** (Ember's editor core, a native window), **VS Code**
(the computer's `code serve-web` wrapped in Ember's IDE window, OSE runtime by default, D10), and
**JetBrains Gateway** (a command to the Gateway installed on that computer). This is a correction
of the mockup, not a new decision.

**Keymap.** [user, 2026-10-25: "단축키는 vscode로 할건지 jetbrains로 할건지 선택할 수 있게 해주면 될거같아."] The Ember editor's shortcuts follow a keymap the user chooses in settings, VS Code or JetBrains. The choice changes the shortcut hints shown and the keys the editor handles (`FR-L30`).

**Rejected.** Cursor and Zed; VS Code over Remote SSH.

### D33: A computer monitor opens from a project's computer

**Decision.** [user, 2026-10-25: "프로젝트에 있는 컴퓨터 누르면 컴퓨터의 CPU, GPU, RAM, 디스크 사용량을
작업 관리자 처럼 띄우는 기능을 추가하도록 하자."] Clicking a computer on the project screen opens a
task-manager sheet laid out after the owner's reference (ref7, Mole's Status screen): CPU, GPU,
memory, disk, network, temperatures, fans and a process table, with members' agent processes
highlighted. Network, temperature and fan cards and the process highlighting are [provisional]
additions from the reference. ember node collects the values (`FR-X7`).

**Why.** Agents run out of memory and load computers; the owner wants to see why from the place
where computers are assigned.

**Rejected.** A separate monitoring app; showing only reachability (`FR-L3`).

### D34: Version control is in the app, at GitHub Desktop level

**Decision.** [user, 2026-10-25: "프로젝트별로 Git을 볼 수 있도록 지원하되, vscode처럼 부족한 UI 말고
Github Desktop을 대체할 수 있을 정도의 체계적인 UI가 필요해."; "프로젝트에서 바로 버전 관리로 이동할
수 있는 버튼 제공하면 좋을거 같고, 세션에서도 켜볼 일이 많을거 같아서 거기도 버튼 하나 두고 거기는
팝업으로 뜨게 하면 좋을거같아."] 버전 관리 is a menu entry with a per-project screen: changes with
line staging and commit, history graph, branches, pull requests with checks and reviews, stash,
conflict resolution, push/pull/fetch, and a team view of each member's branch and worktree. The
project screen links to it; the session header opens it as a sheet. The section list and the team
view are [provisional] details. `DESIGN.md` §6.6.

**Why.** Agents work on many branches and worktrees at once; the owner reviews and merges their
work and should not need a second app for it.

**Rejected.** A VS Code style source control list; sending the user to GitHub Desktop.

### D35: The logo is a phoenix after DarkPyonix's temporary logo (pending final sign-off)

**Decision.** [user, 2026-10-25: "엠버의 로고 디자인은 DarkPyonix의 임시 로고를 참고하도록 해. ... 이거로
부터 영감을 받아서 벡터로 예쁘게 잘 구현해봐. 데스크탑의 작업 표시줄 아이콘은 vscode 처럼 배경 없이
구현하고, 모바일과 앱 내 아이콘은 배경 넣어서 구현해줘."] The Ember logo is a vector phoenix drawn
from DarkPyonix's temporary logo: no background on the desktop taskbar, a background on mobile and
in-app icons. The owner chose candidate B, "phoenix-fold" ("그래 뭐 주 후보인게 젤 좋긴 한거같다.")
and asked for a rounder tail, VS Code's size and more colour layers; those were made and passed an
independent review. Files: `.scratch/design-mockup/logo/phoenix-fold/`.

[user, 2026-10-25: "엠버 로고가 완전히 마음에 들진 않아. 내가 로고 좀 더 동그랗게 하라고 했는데 제대로 반영도 안하고. 일단 지금꺼라도 해서 작업 시작해봐"] **Phoenix-fold B (round 9, the pink band left at 12%) is the temporary
logo.** It is in use in the app and the mockup now, so work can start on it. The owner is not
satisfied with it, so the final logo stays open. Reason: the owner wants the work to start without
waiting for a finished logo. The follow-up is a Draft in `SPEC.md` (FR-L29): make the whole
silhouette rounder. It is not scheduled; it starts only when the owner asks again.

**Rejected.** The first four candidates (ring, drop, ember coal, E) ("여전히 처참한거 같아. ... 내가
넣어준 임시 로고가 훨배 나아."); a new symbol unrelated to DarkPyonix.

### D36: The main server is the relay; the transport moves to letify-link

**Decision.** [user, 2026-10-30, answering "채택, SPEC에 반영"; the opinion was relayed from the letify side] "Ember에 relay가 필요하면 main server가 그 역할을 하면 됩니다. main server는 관리하는 기기이고, 위치가 바뀌지 않습니다. 각 기기는 main server와 홀펀칭으로 연결합니다. 기기끼리 직결이 안 될 때는 main server가 중계합니다. 그래서 relay.darkpyonix.dev 같은 별도 relay VPS를 운영할 필요가 없어집니다." The transport moves from iroh (QUIC) to **letify-link**, a Rust/tokio crate: Noise IK sessions over TCP subflows and UDP, TCP hole punching, path migration between TCP and UDP, its own stream multiplexing, ed25519 node keys reusable from the iroh NodeId, version negotiation, rendezvous as a trait, relay as a role any node can run, and relay-to-direct promotion with hysteresis. Spec draft: DarkPyonix/letify#76. **Update 2026-11-04 (#229)**: letify-link has shipped and is the default backend [provisional]; iroh stays behind its feature for one release [provisional], then is removed (follow-up issue).

**Previous design.** D8 and `FR-N2`: `darkpyonix.dev` ran an iroh-relay (`relay.darkpyonix.dev`) and an address directory, and iroh's QUIC hole punching fell back to that relay.

**Why.** No relay VPS has to be run, paid for or defended. TCP is tried first, so the transport works on networks that block UDP, where iroh can only reach the relay.

**Consequences.**
- Relayed throughput is capped by the uplink of the main server.
- The main server must be reachable by every device: a public IP, or port forwarding, UPnP or PCP. It also does public-address discovery (the STUN or QAD role) for the devices.
- A device pair that cannot connect directly and cannot reach the main server has no path. There is no public fallback relay.
- The `darkpyonix.dev` hub keeps browser ash sharing and the pkarr rendezvous (account directory); it no longer relays Ember traffic once the swap is done.
- Switching is staged: both transports exist behind `FR-N5`, and the swap uses version negotiation so devices on either side keep working during the rollout.

**Rejected.** Running `relay.darkpyonix.dev` as a separate relay VPS (the previous design). Deleting the iroh backend in the same release that makes letify the default *[provisional]*: one release of overlap keeps a rollback path.

**Main server defaults** *[provisional, #229]*. The relay and the address observer start with the server on port 7443 (TCP and UDP), overridable by environment; nodes read the relay URL from the main server's hub device record first, with `EMBER_RELAY_URL` as the explicit override; `derive_relay_url` and the `relay.<host>` derivation are removed from `ember-hub`.

### D37: The server API is authenticated by device key and local token

**Decision.** [user, 2026-11-06, relayed by the supervisor for issue #338: the model is "기기 키 + 로컬 토큰" (device key plus local token)] Three parts, applied to every HTTP and WebSocket route of the server:

1. A remote device authenticates with its letify-link device key (the Noise IK handshake). A revoked device is dropped within one heartbeat (`FR-N3`, #229).
2. The app and tools on the same computer authenticate with a token file in the data folder, mode 0600. The server creates it if it is missing. The app reads it and sends it as a header.
3. Origin and Host checks are added on every route. Loopback alone is no longer trusted.

A request that fails is answered with 401, and tests fix this (`FR-S9` to `FR-S11`, `FR-N7`). Status: built. P1 (token file, middleware, Host and Origin, CDP nonce) #342, P2 (client and app header) #347, P3 (remote device path, `fr_n7_*`) #361, and #368; CI runs the `fr_s9_*`, `fr_s10_*`, `fr_s11_*` and `fr_n7_*` tests.

**Why.** The server API trusted whoever reached `EMBER_LISTEN`, so any local process, any web page that opens a WebSocket to loopback (browsers apply no CORS to WebSockets) and, on a non-loopback address, the whole LAN could use every route, including agent exec, session input and terminal input.

**Consequence and owner decision [user, 2026-11-06, relayed by the supervisor: "지금은 전체 권한, 범위는 나중에" (full authority for now, scope later)].** A device admitted by the gate is trusted with the server user's full authority in v1. It can create a session and run an agent shell as the server user; that shell can read `<data dir>/server-token` and use the local door, including `/api/devices`; it can also reach `POST /api/computers`, the MCP registry and `/api/accounts`. The trust model is that of an SSH key: every admitted device is assumed to be the owner's own. So the split between the local door and the remote door guards against browsers, other local processes without the token and unadmitted peers, not against an admitted device. A per-device scope (for example read-only or chat only) is a later step, tracked in #366. Revoking a device closes its connections and refuses new ones, but sessions it created keep running (v1 allowed: there is no `created_by` column and the sessions belong to the server user; follow-up #369). Found by the P3 review of #338. [provisional] The same hole is open to any process that runs as the server user, a prompt-injected agent included: mode 0600 stops other OS users, not the server user's own processes. None of the options in #338 closes it; it needs agent sandboxing, an OS-user split or an owner-presence check, and is tracked in its own issue.

**Non-loopback owner listener.** [provisional] (leader, 2026-11-13) The owner listener (`EMBER_LISTEN`) speaks plain HTTP, so on a LAN address the bearer token crosses the network in clear. The server therefore refuses to start when `EMBER_LISTEN` is not a loopback address (`0.0.0.0`, `::`, a LAN address, an IPv4-mapped IPv6 address) unless `EMBER_ALLOW_NON_LOOPBACK_LISTEN=1` is set, and logs a startup warning when it is set. Remote devices are meant to come in through the transport door, which checks the device key. This matches the guest door's `EMBER_GUEST_LISTEN_ALLOW_REMOTE`. Rejected: a warning alone (an accidental `0.0.0.0` would expose the token silently in logs nobody reads); removing the option (some owners run behind a TLS proxy or on a trusted host-only network). Tests `fr_s10_non_loopback_listen_*`.

**Rejected.** Loopback trust alone, which is today's model and is documented in `docs/guide/en/install.html` as "anyone who can reach `EMBER_LISTEN` can use every route"; a per-process secret; session tokens only. The issue lists the last two as options and gives no reason against them, so none is recorded here.

**OSE socket relay and connection token lifetime (C2, #458, #461).** [provisional] The OSE process accepts only one connection token for its life (`--connection-token-file`, read once at start). Option B (minting a fresh OSE token on every `GET /api/sessions/{id}/ide` call) is **rejected**: starting a new OSE process per launch would drop every other open window on that folder and break one-process-per-folder reuse, and rotating the token file while running is not supported by OSE and would break reconnects (`reconnectionToken`) for existing windows. Instead, Ember uses **C2 (server-side token substitution on the authenticated relay)**:
- `OseManager` keeps the real 64-hex token in the 0600 token file and in server memory only; it never leaves the server.
- `GET /api/sessions/{id}/ide` returns `relay: "/api/computers/{computer}/ide/relay/{launch_id}"` and a per-process 64-hex `placeholder` as `connection_token` (alongside `endpoint` for one release of backward compatibility).
- `GET /api/computers/{computer}/ide/relay/{launch_id}` (behind `auth::protect` on the local listener, `check_origin`, and the `FR-N3`/`FR-N7` device gate on the transport; never on the guest door) upgrades to a binary WebSocket, dials the launch's `127.0.0.1:<port>`, and in the first 8 KiB of client-to-OSE bytes replaces every occurrence of the 64-byte ASCII `placeholder` with the real 64-hex token (including across frame boundaries). Revoking a device (`FR-N3`) drops its relay streams within one heartbeat, and the device never held the real OSE token.

### D38: Ember 에디터 = the functions of Code-OSS (OSE), the design of the mockup

**Decision.** [user, 2026-11-06: "내가 요구하는건 기능은 oss랑 동일하고 디자인은 목업인거란다 제발 좀 똑바로 일하자."] The Ember 에디터 window does everything Code-OSS (the OSE runtime the server starts) does, and looks like the mockup `.scratch/mockup-v8/editor.html` with `editor.css`: the island style of D25 (rounded island panels for the code area, tabs and tool windows, 40 px icon-only tool-window stripes, breadcrumbs under the tabs), with the light and dark tokens of the mockup's `style.css`. Function parity is tracked row by row in `docs/design/EDITOR-PARITY.md`.

**What went wrong, and the correction.** The leader's brief of 2026-11-05 read D23 as the design source for the window and had it drawn as a Code-OSS look-alike workbench (dark `#1e1e1e` chrome, the vendored Code-OSS CSS) in place of the island layout. D23 decides only *how the chrome is drawn* (rsx with the vendored Code-OSS CSS, built by script): it says nothing about the *design*, which is D25 and the mockup. `FR-L31` had been edited to say the island layout was replaced; that edit was wrong and is reverted by this decision. The Code-OSS CSS stays as the source of behaviour classes and tokens where the mockup uses the same widget; it does not set the look.

**Rejected.** A dark Code-OSS look for the window (the owner asked for the mockup). Leaving the island layout "as a later polish" (the owner's words are a requirement, not a polish item).

**Why.** The owner wants one product: the editor's behaviour is what developers know from VS Code, and its look is the same Ember design as every other screen. The keymap menu stays where `FR-L30` puts it (the ⋯ menu of the top bar).

### D39: A new computer joins with a one-time enrol code, not a pasted token

**Decision.** [provisional, session pZ proposal, needs supervisor confirmation] Adding a computer is a short flow: the server mints a one-time code, the user runs `ember-node enrol <code>` on the other machine, and the machine registers itself and appears online. The requirements are `FR-C1` (minting and storing the code) and `FR-C2` (redeeming and status), issue #491. These are the add-computer rows; the TODO in `project.rs` that cites `FR-X7` cites the metrics requirement by mistake.

- The code is minted only from the local admin router (token-protected, never the guest door), is single use, lives at most 10 minutes, carries 244+ bits of randomness and is stored only as a SHA-256 hash. Redeem attempts are rate-limited per source address, and a used or expired code gets the same generic refusal.
- The existing hub device-link route (`GET /api/hub/devices`, `POST /api/hub/devices/{id}/computer`) stays the first step for devices already signed in to the account and needs no new server code. The code flow is for machines that are not signed in.

**Rejected** *[provisional]*. Pasting a long-lived node token by hand (today's `POST /api/computers {name, url|peer, token}`): the token is long-lived, travels through chat or a clipboard, and is easy to leak. A reusable or non-expiring code, which is the same leak with a shorter string. Requiring every computer to sign in to the GitHub account first, which fails for headless machines.

**Why.** A code that dies after one use and ten minutes leaves nothing worth stealing, and the server never keeps it in a form that can be replayed.

**Confirm at join (leader approval 2026-11-06).** [provisional, 2026-11-06, leader approval relayed by the supervisor; owner confirmation pending] The independent review of #523 (issue #526) found that a hostile or confused node that redeems a code would get what D37 grants (file read, agents, so remote execution) with no owner act. The door design memo (`.scratch/pz/enrol-door-design.md`) answers it, and the leader approved these decisions. The labels D1 to D13 are the memo's.

- **D4, D5: the owner confirms every enrolment.** A redeemed code makes the enrolment `awaiting_confirmation`. The app shows the machine's reported host name and the observed address next to the owner-chosen name. The server mints the node token only when the owner presses confirm, and delivers it once. The owner can deny. An unconfirmed enrolment expires 10 minutes after the redeem (equal to the code lifetime).
- **D3: the credential is server-minted and released after confirm.** The redeem answer carries only `{enrol_id, poll_secret}`; the node polls with the per-enrol secret and receives the token once.
- **D1, D2: the enrol door is its own listener**, `EMBER_ENROL_LISTEN`, default OFF, with `EMBER_ENROL_PUBLIC_URL` in the printed command. When the door is off the mint answer says `door: off` and prints no command.
- **D6: limits.** A global failure cap of 60 per 10 minutes across all sources, door limits of 5 per second with burst 10, 8 in flight, 32 connections and a 4 KiB body, all in a configurable struct.
- **D9: `ember-node enrol` asks before it redeems** (prompt unless `--yes`). A server address outside the private ranges needs `--allow-http` to use plain http; plain http on a private LAN is allowed. Both the node and the app warn that plain http exposes the code on the network.
- **D10: no enrol over the transport.** A machine that already has a device key uses the hub device-link route.
- **D13: the node keeps its credentials in `enrol.json`** (0600, in its state dir); `EMBER_NODE_TOKEN` wins when set.
- **D11 [provisional, pT/pX].** When the node reports no address, the server proposes the observed address with port 8741 and shows it to the owner for editing; the guess is never trusted automatically.
- **Later, not v1.** D7 sealed credential delivery (X25519 with an AEAD). D8 server fingerprint pinning (`--server-fingerprint`) is built. D12 door auto-close is built and is the default; `EMBER_ENROL_AUTO_CLOSE=0` opts out (`FR-C2l`, #539) [provisional] (leader, 2026-11-13): there are no production deployments to protect, so the safer door is the default; rejected: opt-in only (an owner who forgets it leaves the door open), and a door that is never bound (the node then has nothing to redeem at).
- **The node may cancel its own enrolment** (`FR-C2m`, #617) [provisional] (leader, 2026-11-13). `DELETE /enrol/status/{enrol_id}` with the poll secret, verify first, the poll's limits and 404; it moves only `awaiting_confirmation` to `expired`. Why: a node that gives up must not leave a request the owner may confirm by mistake. Cost accepted: one more public route on the door, and a state change reachable without the owner (only by the holder of the poll secret, and only toward refusing). Rejected: no route (the owner must deny by hand), and a new `cancelled` state (the app would need a new view for the same outcome as `expired`). The v1 residual risks are documented in `COMPUTERS.md` and `FR-C2j`.

**Rejected at this step.** Releasing the credential at redeem time (a hostile node would have it before any owner act, and a sniffer who wins the redeem would hold it). Holding the redeem request open until the owner clicks (it runs into the request timeout and the in-flight cap, and one valid-looking redeem could pin permits). A node-chosen token (the node would send a secret that controls itself to a server the owner has not confirmed, and a rogue server at the URL would collect a working key to the node). Option (b), enrol over the `FR-N5` transport (it weakens the rule that every transport caller is authenticated, and the machines it would serve already have a device key and use the device-link route). A shared listener, a `/enrol/*` prefix on the guest door (it would stay open as long as the guest door, mixes two audiences and two proxy setups, and a guest-door bug would reach enrol).

**Server peer id in the delivery (C3b).** [provisional, 2026-11-06, leader approval relayed by the supervisor; owner confirmation pending] The `joined` answer carries the server's public transport peer id when the transport is on, and omits the key otherwise. It is public, so it is not a `Secret`. The node cannot verify the server, so the value is trust on first use and must not widen the node's reach unchecked. Node rules, required and tested in the node step (`ember-node enrol` is not built): R1 pin first (`--server-fingerprint` from the app's command is compared before anything is written; a mismatch aborts and writes nothing; the pin the app puts in the command is the FULL 64-hex server peer id, because the 16-hex short form is only 64 bits and a determined attacker can grind a key with a chosen prefix: the short form is accepted when a person types it, with the notice "short pin: 64 bits", and is for screens, not for commands) [supervisor, 2026-11-06, security-strengthening correction of the first R1 text]; R2 no pin means no automatic trust (the node asks, and non-interactive use needs `--server-fingerprint` or `--trust-unverified-server`); R3 verify, then write `enrol.json` and `allowed-peers` together, shape-check the id, add that one id only; R4 the node prints what it will allow.

**Rejected for the peer id.** A signed delivery: the node has nothing to verify the signature against, so it is the same trust on first use. Adding the delivered id to `allowed-peers` automatically: it widens the node's reach (the transport crosses NAT through relays) on an unauthenticated value. The id in the redeem answer: it widens an unauthenticated answer for little gain; it can be added later with its own test.

**Status.** `FR-C1` (admin side and redeem core) is built and merged (#523); `FR-C2` and its parts `FR-C2a` to `FR-C2k` are specified and approved, not built (issues #491, #526).

### D40: The Gemini CLI is a harness through the ACP adapter

**Decision.** [user, 2026-11-06, option "(a) Gemini CLI 어댑터 추가" chosen by the owner] Ember adds the Gemini CLI as a harness on top of the generic ACP adapter (`ember/server/src/agents/acp.rs`, issue #53). The mockup's Gemini card and the Home chip stay. Until the adapter is verified against the real CLI, the app shows the card as "preparing" (준비 중입니다). The requirements are `FR-A10` to `FR-A10g` (issue #53).

- [provisional] The harness is an ACP agent named `gemini` with a built-in preset (command `gemini`, ACP flag per `SPEC.md` "Gemini CLI facts"), not a new protocol adapter. This keeps `FR-A3`: adding the agent needs a preset only.
- [provisional] Authentication stays the user's own: Ember never stores or reads Gemini credentials.
- [provisional] Usage limits come from the CLI and its documentation, never from an Ember estimate.

**Rejected** (the owner chose adding the adapter over each of these). Removing the Gemini card from the mockup and the Home chip. Keeping the card as a permanent "preparing" placeholder with no adapter behind it.

**Why.** The owner wants Gemini in the harness list as a working harness, and the ACP adapter already exists, so the cost is a preset plus verification.

**Owner and leader decision, 2026-11-06 (option "(c) 카드 유지 + API 키·엔터프라이즈 전용" tag [user, relayed by the supervisor]).** Research found that Google's Gemini CLI announcement says that since 2026-06-18 the Gemini CLI no longer serves Google AI Pro, Google AI Ultra and free-tier individual accounts (replaced by the Antigravity CLI, `agy`); Enterprise Code Assist licences and API-key authentication are stated as unaffected, and Vertex AI is not addressed. Sources, read on 2026-11-06: <https://github.com/google-gemini/gemini-cli/discussions/28017> (announcement of 2026-06-18) and <https://9to5google.com/2026/06/17/gemini-cli-code-assist-shutting-down/> (consumer accounts only, shutdown on 2026-06-18). The decision: the Gemini card stays, it is marked as for API-key and Enterprise (Code Assist) accounts only, and a user with an individual account is pointed to the Antigravity harness (D14). The adapter authenticates with an API key (and Code Assist) over the ACP adapter; "Login with Google" for individuals is not supported. This narrows the earlier choice "(a) Gemini CLI 어댑터 추가": the adapter is still built, for the accounts the CLI still serves. Rejected: building the adapter around "Login with Google" for individuals (the CLI does not serve them); removing the card; merging it into the Antigravity card.

**Status.** Specified only (`FR-A10`, Draft; issue #53). Not built.

### D41: GitHub is the first integration, read through the session computer's own gh

**Decision.** [user, 2026-11-06, option "(GitHub부터)" chosen by the owner for #498] The home screen's 연동 area starts with one integration, GitHub, in the November scope. Calendar, chat and mail integrations stay specified only (`FR-L23`).

- [user] The GitHub card shows what can be read now: the number of repositories, open pull requests, reviews waiting for the signed-in account, the CI pass rate of the last runs on the default branch, and the work a CI failure can start (a routine linked to a failing CI, `FR-L25`).
- [provisional] Authentication is the `gh` login on the computer that runs the project's sessions, the same path the 버전 관리 screen uses (`FR-G8`). Ember never stores a GitHub token and has no OAuth flow of its own; when `gh` is missing or signed out the card says so and offers no connect button that would pretend otherwise.
- [provisional] The server answers from `gh` output on demand with a 90 second cache and one request in flight, never from a background poll. A value `gh` cannot give is null and the card shows "알 수 없음", never an invented number.
- [provisional] Text that comes from GitHub (repository names, account names) is untrusted: it is cleaned of control characters and cut before it is shown; titles and bodies are not part of this card.

**Rejected.** Building the calendar, chat and mail integrations first (the owner chose GitHub). An Ember-owned GitHub OAuth app that stores tokens on the server (a second credential store, and the user's `gh` login already covers it). A demo-only card with invented connection state (`NFR` "no value is invented").

**Why.** GitHub data is already reachable through the same `gh` path as the pull request screen, so the card costs an API and a view, not an identity system.

**Status.** Specified (`FR-L35`, `FR-G13`, #498). The server API is in progress; the card is built by the Home owner against it.

### D42: Per-node OSE deployment for the Ember editor

**Decision.** [provisional, #457] When the Ember editor opens a session whose computer is a registered node, the OSE runtime (`dpx-ose-server`) runs on **that node**, spawned and managed by `ember-node` (`FR-W4a` to `FR-W4c`), not on the main server against the project mount.

- [provisional] `ember-node` manages one OSE process per canonical workspace folder under its allowed roots (`PathPolicy`), passing `--server-data-dir <state_dir>/ose-data`, `--extensions-dir <state_dir>/ose-data/extensions` and `--connection-token-file <state_dir>/ose-data/tokens/<hash>.token` (mode 0600, deleted on exit), with the same cleared environment allowlist (`HOME`, `PATH`, `LANG`, `LC_ALL`, `TMPDIR`, `USER`) and 120 s extension-install rules as the server's local `OseManager` (#459, #461).
- [provisional] `ember-node` exposes three authenticated routes behind the node bearer token (and the peer allowlist over the `FR-N5` transport): `POST /ide/ose` (`{folder}` -> `{launch_id, port}`), `GET /ide/ose/{launch_id}/relay` (WebSocket binary byte relay to the node's `127.0.0.1:<port>` OSE listener, where the node performs the C2 placeholder-to-real-token substitution in the first 8 KiB of the client-to-OSE stream so the real OSE connection token never leaves the node), and `POST /ide/extensions` (`{id, version?}` -> `{installed, id, output}`).
- [provisional] `ember-server` relays `GET /api/sessions/{id}/ide`, `GET /api/computers/{computer}/ide/relay/{launch_id}` and `POST /api/computers/{computer}/ide/extensions` to the target computer's `ember-node` over HTTP or the `FR-N5` peer-to-peer transport. The editor client always connects to the main server's `/api/computers/{computer}/ide/relay/{launch_id}` and never holds a node credential or dials a node directly.

**Rejected** *[provisional]*. Option (b), running one OSE on the main server against the project mount (`ember/server/src/computers/mount.rs`):
1. The project mount exists only while a Claude Code turn is live (`COMPUTERS.md` "Project mount / When"); opening the editor on an idle Claude session, a Codex session, or a server built without `mount-nfs`/`mount-fuse` finds no mount at all.
2. Language servers (`rust-analyzer`, `pyright`, `tsserver`), formatters, linters and build tasks spawned by the OSE extension host would execute on the main server's CPU, OS and toolchain instead of the node where the project's compilers, virtual environments and SDKs live.
3. The project mount has a 1 s attribute/listing cache (`EMBER_MOUNT_TTL_MS`), no file-watcher change feed (`inotify`/`FSEvents` do not cross the mount), no exclusive rename, and is not compiled or verified in CI.

**Why.** Running the OSE on the node puts `remoteFilesystem`, native file watching (`fs.watch`), language servers and extension hosts on the same machine and filesystem as the project and the node's persistent terminals (`§P`), while the server relay keeps authentication, device revocation (`FR-N3`) and the guest-door exclusion (`FR-T14`) at one boundary.

**Status.** Specified (`FR-W4a` to `FR-W4c`, Draft; issue #457). Local server OSE and its relay are in #458/#459/#461; node OSE routes and server-to-node relay follow this spec.

### D44: Webviews, custom editors and notebook outputs in the Ember 에디터: native for the common cases, VS Code for the rest

**Decision.** [provisional, #452, needs the owner's confirmation] The Ember 에디터 window hosts no webview, keeping D9a. Markdown preview, image files and notebook outputs that are text, stream, error or image mimes are drawn natively. Every other extension webview panel, webview view, custom editor and notebook output (HTML, JavaScript, widgets) shows a placeholder in its own panel, with the extension's name and `VS Code에서 열기`, which opens the project in the VS Code target (`ember/editor/src/fallback.rs`). FR-E5 ("Extension webview panels still work, contained to their panel") is met through that fallback, not inside the window (`FR-E8`).

**Rejected** *[provisional]*. A platform webview region inside the Ember 에디터 window (E5 allows it, D9a does not; it needs a webview host per OS that does not exist (#30), a secure webview origin, a native view composited into dioxus-compose, and a JS engine in the editor window). Leaving these features out (D38 asks for function parity). The options and their costs are in `docs/design/EDITOR-PARITY.md`, "Webviews, custom editors and notebook outputs".

**Why.** Extension webviews are extension programs written for a browser engine; they cannot be drawn without one. The cases users meet daily (reading markdown, looking at an image, a notebook's printed output and plots) need no extension code, so they can follow the mockup's design natively, and the rest still works one click away in VS Code Web, which already is the IDE window exception of E1.

**Status.** Proposed, not built. Waits for the owner; if the owner wants option A, D9a changes first.

### D45: Local secret files: swept at startup, redacted logs, a week of retention

**Decision** *[provisional, 2026-11-13, #521, #544]*. Files that hold or may hold a secret on the server machine follow three rules. (1) OSE connection token files left by a killed server are removed when the next server starts (`OseManager::sweep_stale_tokens`), not only when the same folder starts again. (2) The agent stderr log of the session shim (`<shim dir>/<id>.log`) is written line by line through the upstream redaction without the 300-character cap, as a 0600 file in a 0700 directory. (3) A log of an ended shim is kept for 7 days and then removed when the next shim starts.

**Rejected** *[provisional]*. Leaving stderr raw and only restricting the mode (a log the owner pastes into an issue would carry a key fragment). Capping log lines at 300 characters like events (a debugging log loses the part that explains the error). Deleting the log with the shim (the log is needed after a crash). A Windows ACL for the token file (the server's OSE start runs on unix first; documented as default ACL).

**Why.** A SIGKILL skips every drop handler, so only a startup sweep bounds the life of a dead token. The redaction is a heuristic, so the mode stays private as well. Seven days covers a debugging session after a weekend and keeps the directory from growing without bound.

### D46: Node file operations open first and check the handle; the write relay waits for the namespace operations

**Decision.** [provisional, #468, 2026-11-13] The node's content file operations (read, pwrite, setattr, grep, and on Unix the atomic write) open first and check the opened handle against the path policy (resolve again, and on Unix compare device and inode), with `O_NOFOLLOW` on the final component and, for the atomic write, a checked directory handle (`openat`, `renameat`). Namespace operations (mkdir, remove, rename, symlink, stat, lstat, readlink, list, glob) keep acting on paths for now. The server write relay may relay only the covered content operations until those move to directory handles too.

**Rejected** *[provisional]*. Accepting the race under D37 without code (an admitted device has full authority, but the local node and denied paths narrow the API on purpose, and a narrowing that a symlink swap defeats is not one). Resolving every component with `openat` from the root on every call (correct, but a rewrite of every operation; it is the next step for the namespace operations). Linux-only `openat2(RESOLVE_BENEATH)` (not on macOS or Windows).

**Why.** The check-then-open gap is reachable remotely through the server relay (#463). Checking the handle closes it for every operation that reads or writes content, with no change to the API.

**Status.** Written, not yet compiled or run (`ember/node/tests/fs_toctou.rs`). SPEC `FR-X1` (open, then check, #468).
