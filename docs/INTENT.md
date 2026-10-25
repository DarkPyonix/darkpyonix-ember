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
| **E1** | The client's launcher and conversation screens never contain a webview, under any circumstance. They are built on `dioxus-compose`. The IDE window is the one place a webview may exist. | [user, 2026-10-03] |
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

**Consequence.** This also settles the HTTPS problem `ember/proxy/` has had for phones and tablets
(`ember/proxy/docs/BACKGROUND.md` §7-1), whichever transport is chosen [provisional].

### D9: The client is native (dioxus-compose); the IDE window is wrapped VS Code Web

**Decision.** [user, 2026-10-03] The launcher and the conversation screens are built on
`dioxus-compose` with no webview (E1). The IDE window is a webview running VS Code Web through
Ember's wrapping layer, whose current implementation is `ember/proxy/` (merged in #1).

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

- D9: the tabs are native `dioxus-compose` screens, with no webview (`NFR-L2`).
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

**Status.** Specified only (`FR-L10`–`FR-L13`, `FR-T10`–`FR-T17`, `FR-U7`). Not built. Still open: how an invited person authenticates (Q10, remaining part) and the exact JSONL layout of `FR-T16`.

## Open questions

| ID | Question | Status |
| -- | -------- | ------ |
| **Q1** | How do a project's computers relate: copies of the same workspace (the same repository checked out on a Mac and a Linux box), machines with different roles (iOS builds on the Mac, GPU work on Linux), or simply "whichever computer I am at"? | Open; decides how much of a session's observations survive a move |
| **Q2** | Who moves a session: the agent (a `switch_computer` tool plus a routing policy, e.g. "needs CUDA → GPU box"), the user, or both? | **Closed** [user, 2026-10-25]: the user designates the computer and the server falls back and returns on connection changes; see D20 |
| **Q3** | When a session moves, does an open IDE window follow it, or stay with its computer? | Open |
| **Q4** | How are a transcript's computer-specific observations invalidated on a move? Proposal: split the transcript into a computer-independent part (intent, decisions, plans, conclusions) and a computer-specific part (file contents, command output, paths, environment, background jobs); record each file observation as (path, content hash, computer, time) and re-hash on the new computer so only changed files are flagged; keep the environment description in one replaceable block instead of appending; scope "read before edit" to a computer. | [provisional] proposal. v0 (a system notice on a switch) is implemented (#34, SPEC `FR-S7`); the hash comparison is written from the tool-call log (SPEC `FR-S7`, #79); a separate recorded (path, hash, computer, time) table was not built, since the log already names the files |
| **Q5** | A job started on computer A when the session moves to B: kill it, keep it and notify on completion, or block the move? Proposal: keep it running and notify. | [provisional]; built as proposed in ember node (`FR-X4`, `ember/node/src/jobs.rs`) |
| **Q6** | For each wrapped CLI, where exactly is the interception point (shell, PTY, filesystem or tool protocol) that keeps its native behaviour intact? Candidates per agent: the vendor's own headless protocol, or the Agent Client Protocol (ACP), which OMP exposes natively and Claude Code / Codex / Gemini reach through adapters. | **Decided per agent:** Antigravity [user] (D14, implemented #64); Claude Code and Codex [provisional], implemented (#14); OMP and other ACP agents [provisional], implemented (#61), real OMP not run (#53); Gemini open. Details: **Claude Code:** `--print` stream-json with `--permission-prompt-tool stdio` (#14). **Codex:** `codex app-server` directly (#14); the maintained ACP adapter (`agentclientprotocol/codex-acp`) is itself a translation layer over app-server, so it can only lose detail (approval choices, turn steering, thread ids, usage), and the older one compiles Codex internals pinned to an old release, breaking FR-A1. **Antigravity**, decided in D14 [user]: `agy -p … --output-format stream-json`, one process per turn, `--conversation=<id>` for native resume (agy 1.2.10, recorded in `ember/server/tests/fixtures/agy/`). Print mode cannot ask for a permission: it soft-denies the tool, and a `PreToolUse` hook answering `allow` (even with `permissionOverrides`) did not lift that (recorded). So Ember runs agy with `--dangerously-skip-permissions` and makes its own `PreToolUse` hook the only gate: the hook (`curl` to a local route holding a per-run secret) blocks until the user answers, `deny` hard-blocks, an unreachable server denies, and before every turn `agy -p /hooks --output-format json` must list the hook or the turn is refused (D14; tested on 1.2.16 in SPEC §A). The hook, a rules file (instructions) and `mcp_config.json` live in a per-session folder passed with `--add-dir`, agy's own customization root (`.agents/`); nothing in agy or in the user's `~/.gemini` or project is changed. Costs: the folder shows up to the model as an extra, first-listed workspace (the rules file tells it to stay out), the stream carried no tool output on 1.2.10 (1.2.16 has it), and agy has no config-dir variable, so Antigravity accounts (D7) are not possible yet. Rejected: hooks in `~/.gemini/config/hooks.json` (shared by every agy run and cannot hold per-session MCP servers), hooks in the project's `.agents/` (writes into the user's repository), `--input-format stream-json` (one process per session, but its input schema is undocumented). **OMP and other ACP agents** *[provisional]*: the agent's own ACP server over stdio, through one generic ACP client (#53, ACP protocol v1); OMP's is `omp acp` (oh-my-pi v18.5.0, read from source, not yet run). Ember is the ACP client, so file reads/writes and shell commands that the agent routes through `fs/*` and `terminal/*` are executed by Ember itself, which is the interception point: they run on the session's computer through its node, with no shim. Tools the agent does not route through ACP (search, LSP, …) still read the main server's filesystem, which the project mount covers. Rejected: OMP's own `--mode rpc` NDJSON protocol (OMP-specific; ACP serves every ACP agent with one adapter). Claude Code / Codex through ACP adapters stays rejected as above. Others (Gemini): open. |
| **Q7** | Which transport: iroh, rustunnel, a tunnel written in Rust from scratch, or tailcat? | **Decided, conditionally** [user, 2026-10-03]: **iroh 1.0**, behind a replaceable interface (SPEC `FR-N5`); if it misses `NFR-N1`, our own implementation is evaluated. Implemented in `ember/transport` (#23); `NFR-N1` not yet measured on real networks (#10). tailcat was withdrawn after the user's objection (two apps on mobile). An own implementation is weeks rather than months for the basic path, since a failed hole punch falls back to the relay; it stays the replacement option. `rustunnel` is not a transport: it is a server-relayed tunnel with no hole punching, and AGPL-3.0, so it must not be linked into clients; it is only a reference for the hub's public HTTPS edge. Source: darkpyonix leader, core PROJECT Q1 (3ea7fb7). |
| **Q8** | Is the Tauri scaffold kept long-term, and for what? | **Closed: deleted** [user, 2026-10-03] |
| **Q9** | `ember/proxy/`'s open items carry over: HTTPS for phones (likely resolved by D8), login being a thin shell, and the pre-distribution security holes in `ember/proxy/docs/BACKGROUND.md` §7-3. | Open; HTTPS over P2P is #10 |
| **Q10** | How does an external account (another person) authenticate to a channel, and what may it see: only that channel, or also the project's sessions, files and computers? | **Decided** [user, 2026-10-25]: "그 채널만 볼 수 있어. 디스코드나 슬랙 생각하는게 편할거야." An invited person sees only the channels they are invited to (`FR-T14`). **Open remainder:** the authentication mechanism of the invite |
| **Q11** | Is Home's general chat a session without a project (so it needs a computer, an account and a place for its transcript), or something else? | **Closed** [user, 2026-10-25]: "분류되지 않은 프로젝트로 할당하도록 해. 나중에 프로젝트로 옮길 수도 있겠지." Home chat sessions live in a built-in Unsorted project (`FR-L13`) |
| **Q12** | Do channels persist across machines: stored on the main server and shared by every client and computer, or per client? Where do invited people's messages live when the main server is self-hosted? | **Closed** [user, 2026-10-25]: "음 메인 서버에 사람 전용 대화 jsonl 파일이 필요하려나" Per-channel append-only JSONL on the main server (`FR-T16`); the exact layout is [provisional] |
| **Q13** | Which tabs follow Home, Project and Channels ("등으로")? And is a channel per project only, or can one span projects? | **Closed** [user, 2026-10-25]: "탭은 기능으로 올라올 필요가 있으면 더 올라오면 되는거야. 채널 하나가 여러 프로젝트에 걸칠 수 없어. ..." Full quote in D22. A channel belongs to one project; every project has a default channel (`FR-L12`, `FR-T10`) |

### D23: Ember vendors Code-OSS CSS by script and builds the workbench chrome rsx

**Decision.** [provisional, leader decision 2026-10-25, user confirmation pending] Ember carries the Code-OSS CSS the workbench chrome needs, in `ember/app/assets/code-oss/`. `ember/ose/vendor-workbench-css.sh` fetches it from the Code-OSS commit the OSE build pins (`PINNED_COMMIT` in `ember/editor-conn/src/lib.rs`, tag in `ember/ose/VERSION`) and records the commit hash and the MIT notice in every file. Ember's only edit is `workbench-has.patch`, which turns `:has()` rules into state classes (#142). Ember builds the workbench chrome rsx (tree rows, view filter, title bar command center, action widget) and sets those classes (#143). Hover and chat markdown are drawn with `dioxus-compose-markdown` Compose widgets; there is no HTML `.rendered-markdown` path, so `renderedMarkdown.css` is not vendored.

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

**Status.** Specified only (`FR-F1`–`FR-F9`, Draft; issue #152). Not built. The Files menu mock is separate UI work.
