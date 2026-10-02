# SPEC.md — DarkPyonix Ember

Functional (`FR-*`), non-functional (`NFR-*`) and protocol (`PR-*`) requirements. Every
implementation traces to an ID here; a requirement with no test is not done.

> **Revised 2026-10-03** to follow `INTENT.md`'s conversation-first, main-server model. The 09-22
> launcher section (§L) is rewritten. The IDE window (§W), its bridge (§B), the kernel (§K) and
> the editor-core draft (§E, formerly §M) are kept with small changes.
>
> Tags: **[user]** — from the user's brief. **[provisional]** — team proposal, not confirmed by
> the user; may be overturned. Requirements without a tag follow directly from a `[user]` decision
> in `INTENT.md`.

Areas: **L** launcher and conversation UI · **S** sessions and transcripts · **A** agent wrapping ·
**X** execution on computers · **T** agent-to-agent (A2A) · **R** remote and agent browser ·
**U** accounts and usage · **N** networking · **W** IDE window · **B** bridge · **K** kernel ·
**E** editor core (draft).

---

## §L — Launcher and conversation UI (dioxus-compose, no webview: E1)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-L1** | The main screen lists projects as selectable entries. | Renders from cached main-server data on cold start, before any network round trip completes; updates in place when fresh data arrives. |
| **FR-L2** | Each project shows its conversation sessions with a status: running, waiting for approval, finished-unread, finished, failed. | Status changes are pushed from the main server (`PR-1`), not polled per session. "Waiting for approval" outranks "running" when both apply. A finished session stays marked unread until opened. |
| **FR-L3** | The bottom of the main screen lists computers with reachability (online / offline / unknown) and which projects each is assigned to. | The list renders before reachability is known; reachability updates in place. An offline computer stays listed with its last-known state. |
| **FR-L4** | A computer can be assigned to and unassigned from a project; a project can have many computers and a computer many projects. | Assignment persists on the main server; a second client sees the change without reload (`PR-1`). |
| **FR-L5** | Opening a session shows its conversation: messages, tool-call cards, file-change summaries, pending approvals, and the session's current computer and account. | Renders the transcript from the main server's normalised representation (`FR-S2`), identical to what any other client sees. |
| **FR-L6** | The conversation view can send messages, answer approval requests, interrupt a running turn, and queue a message while the agent is busy. | A queued message is delivered at the next turn boundary, in order. |
| **FR-L7** | An **"Open IDE"** button at the top right of the conversation view launches, for the session's project and current computer: Ember's IDE window (§W), VS Code, or JetBrains Gateway. [user] | The launcher process itself never creates a webview (`NFR-L2`). The choice of target is remembered per user. |
| **FR-L8** | A new conversation is started from a project with a chosen agent, account (`FR-U2`), computer and model. | Defaults to the last-used combination for that project. |
| **FR-L9** | Sessions can be pinned, archived, renamed, searched (`FR-S4`) and exported. | Export produces a self-contained file of the normalised transcript. |
| **NFR-L1** | Launcher cold start and idle memory stay within `dioxus-compose`'s own NFR-3 targets (under 100 MB RSS for an empty window) plus Ember's data model. | Re-measured at Ember's M1, not assumed. |
| **NFR-L2** | The launcher and conversation UI never allocate a webview under any code path. | A CI check fails the build if any webview-creation API (`WKWebView`, `WebView2`, `android.webkit.WebView`) is reachable from launcher-process code. E1 made testable. |

---

## §S — Sessions and transcripts (main server: E3)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-S1** | A session belongs to a project and has a mutable *current computer* attribute (`INTENT.md` D1). | Changing the current computer does not change the session's identity, history, account or A2A address. |
| **FR-S2** | Every session's transcript is stored on the main server in one normalised representation, regardless of which agent produced it. Each agent's own native session state (e.g. Claude Code's session file, Codex's thread) is also kept on the main server, so the agent's own resume works. | Restarting the main server loses no completed turn. Resuming a session after a restart uses the agent's native resume (`--resume <id>` for Claude Code, thread resume for Codex). |
| **FR-S3** | A session survives every client disconnecting, and any number of clients can attach to it at once. | Close all clients mid-turn; the turn completes; a later client sees the full result. Two clients attached at once both see live output. |
| **FR-S4** | Full-text search across all sessions' messages. | Indexed search (not a linear scan); results link to the matching message. |
| **FR-S5** | A session can be forked from any completed turn where the agent supports it. | The fork records its parent and turn; agents that cannot fork have the action disabled, not failing. |
| **FR-S6** | Idle sessions release their agent process and reconnect transparently on the next message; a session being viewed is kept alive. | Measured: an idle session's agent process exits after the idle timeout; sending a message restores it via native resume. A session open in a client is not reclaimed. |
| **FR-S7** | *[provisional — `INTENT.md` Q4]* When a session's current computer changes, observations of the previous computer are invalidated. | v0: a system notice tells the agent the computer changed and prior file observations must be re-read before editing. Target: per-file content-hash comparison so only changed files are flagged. |

---

## §A — Agent wrapping (E2: native behaviour preserved)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-A1** | Claude Code, Codex, Antigravity and OMP run on the main server as their own unmodified CLIs. [user] | Each agent's version is the vendor's release; no agent binary or package is patched. |
| **FR-A2** | Each agent is driven through its own non-interactive protocol, chosen per agent: Claude Code via `--print --input-format stream-json --output-format stream-json` with `--permission-prompt-tool stdio`; Codex via `codex app-server` JSON-RPC; Antigravity via its stream-json print mode, one process per turn; other agents via the Agent Client Protocol (ACP) where they support it. | An integration test per agent runs a turn that reads a file, edits it and runs a command, and verifies the normalised events (`FR-A3`). |
| **FR-A3** | All agents' output is normalised into one event model (message, tool call, tool result, approval request, usage, turn end) driving one session state machine. | Adding an agent requires an adapter only; the UI, storage and A2A need no change. Matches `proxy/dpx/agents/`'s adapter rule. |
| **FR-A4** | The agent's own settings, models, modes, session IDs and resume keep working as they do natively. | A session started in Ember can be resumed with the agent's own CLI on the main server, and vice versa where the agent supports it. |
| **FR-A5** | Tool approvals: allow once, always allow (for this session and tool kind), deny; a list of pending approvals; an opt-in mode that approves everything. | For agents with no headless approval channel (Antigravity), approval is obtained through the agent's own hook mechanism (a pre-tool-use hook calling back to the main server), not by patching the agent. |
| **FR-A6** | Supported agents are detected automatically on the main server, with their installed versions. | Detection runs at startup and on demand; a missing agent is shown as not installed, not as an error. |
| **FR-A7** | The main server manages MCP servers centrally and injects them into each agent session at creation. | An MCP server added once is visible to every agent that supports MCP. |
| **FR-A8** | Scheduled tasks: cron expressions (with time zone), fixed intervals, and one-off runs; each run either continues an existing session or starts a new one. Agents may create schedules from within a conversation. | Missed triggers while the server was down are detected and reported, not silently dropped. |

> The 09-22 transcript parsers in `proxy/dpx/agents/` (Claude Code, Codex) satisfy part of
> `FR-A3` for reading existing history and are kept (`INTENT.md` D13).

---

## §X — Execution on computers (D4)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-X1** | Each computer runs one Ember execution daemon that carries out tool actions for sessions (file read/write, directory listing, search, command execution with PTY) and streams results back to the main server. | A CLI on the main server, wrapped, performs a read-edit-run cycle whose effects appear on the target computer's disk and processes only. |
| **FR-X2** | The wrapping is transparent to the agent: paths, working directory and environment the agent sees are the target computer's. | The agent's own "print working directory" and environment queries report the target computer's values. |
| **FR-X3** | A session's current computer can be switched. *[Who triggers it is open — `INTENT.md` Q2.]* | After a switch, the next tool action runs on the new computer; the environment description given to the agent is replaced, not appended (`FR-S7`). |
| **FR-X4** | *[provisional — `INTENT.md` Q5]* A background job started on one computer keeps running after the session switches away, and its completion is reported into the session. | Start a long build on A, switch to B, finish the build on A: the session receives the result. |
| **FR-X5** | The daemon is the only Ember component required on a computer for agent work; it does not run agent CLIs or store transcripts. | Measured: daemon RSS stays bounded and independent of the number of sessions using that computer. |
| **NFR-X1** | *[provisional]* Before and after moving agents to the main server, measure whether local slowdown came from agent runtimes and transcripts (memory) or from builds and tests (CPU). | If CPU dominates, add a concurrency limit per computer on the main server. |

---

## §T — Agent-to-agent messaging (D5)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-T1** | Any session can list other sessions it may message and send a message to one, regardless of agent vendor, computer or account. | A Claude Code session and a Codex session on different computers and accounts exchange a message and a reply. |
| **FR-T2** | Agents use A2A through a small CLI or MCP tool available inside every session, authenticated by a per-session runtime token. | No agent needs modification; the tool is injected (as a skill or MCP server) at session creation. |
| **FR-T3** | A delivered message enters the target session through the same path as a user message, with a header naming the sender session, its project/computer and a reply reference. | The target agent can reply with one call using the reply reference. |
| **FR-T4** | Messages to a sleeping (idle-released) session are queued durably and wake it. | Restart the main server with a message queued; it is delivered after restart. |
| **FR-T5** | Loop protection: per-session send rate and per-pair rate limits within a time window. | Two agents instructed to reply to each other forever are stopped by the limit, with a visible notice in both sessions. |
| **FR-T6** | Users can mention another session from the composer, and can turn A2A off per user or per session. | Off means sends to and from that session are rejected with a clear reason. |
| **FR-T7** | A leader session can spawn teammate sessions, assign tasks, and read a shared task list and mailbox. | Tasks and mailbox are stored on the main server; each teammate keeps its own approvals. |

---

## §R — Remote browser and agent browser use (D6)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-R1** | A browser session can be opened whose network egress is a chosen computer. | A what-is-my-IP page shows the chosen computer's public IP; a page on that computer's LAN or `localhost` is reachable. |
| **FR-R2** | Browser profile data (cookies, storage, logins, history) is stored on the main server and reused whichever computer is the egress. | Log in to a site with egress A, switch egress to B, reload: still logged in. |
| **FR-R3** | Agents can drive the same browser through a browser-automation tool (DevTools-protocol based), and the user can see and take over the page the agent is driving. | An agent fills a form while the user watches; the user takes over to complete a login; the agent continues afterwards. An "agent is active" indicator is shown. |
| **FR-R4** | Browser data can be cleared per project. | Clearing removes cookies and storage for that profile only. |
| **FR-R5** | The remote browser is available from every client, including mobile and web, not only from a desktop app. | Verified from the phone client. |

> How the browser is rendered to the client without breaking E1 (pixel streaming into a native
> surface, or confining it to a separate window that is allowed a webview) is open; it must not put a
> webview in the launcher or conversation screens.

---

## §U — Accounts and usage (D7)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-U1** | The main server holds several accounts per agent (Claude Code, Codex, Antigravity), each with isolated credentials and configuration. | Two Claude Code accounts run sessions simultaneously without sharing credentials or settings. |
| **FR-U2** | A new session is started under a chosen account. | The session's account is shown in the conversation view and cannot silently change. |
| **FR-U3** | Usage per account is recorded and visible; usage routing can choose the account for a new session by policy (e.g. least used, failover when one is rate-limited). | A rate-limited account is skipped by the router and the reason is shown. |
| **FR-U4** | Sign in with OpenAI, with a page that lets ChatGPT usage be consumed in addition to Codex token usage. [user] | *Feasibility of consuming ChatGPT usage outside ChatGPT is unverified and must be checked against OpenAI's terms before implementation.* |
| **FR-U5** | API-key providers (any OpenAI-compatible or vendor API) can be added with keys encrypted at rest. | Keys never appear in transcripts, logs or exports. |

---

## §N — Networking (D8)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-N1** | Main server ↔ computer and client ↔ main server connections are peer to peer, through a Rust-implemented tunnel or an existing mesh network (product unconfirmed — `INTENT.md` Q7). | Works across two different NATs with no port forwarding configured by the user. |
| **FR-N2** | `darkpyonix.dev` provides hole-punching coordination and a relay fallback, so connections need no user configuration. | A new computer joins by signing in; no address or port is entered by hand. |
| **FR-N3** | All connections are encrypted and authenticated per device; a device can be revoked. | Revoking a device closes its connections within one heartbeat. |
| **FR-N4** | Clients reach the IDE window over a secure context, so VS Code Web's service-worker-backed webviews work on phones and tablets. | Extension webviews render on a real phone (not only headless Chromium — see `proxy/docs/CONSTRAINTS.md`). |
| **PR-1** | Main server → client push channel for session status, transcript updates, computer reachability and assignments. | Versioned schema; a version mismatch is detected and reported, not silently dropped. |

---

## §W — IDE window (VS Code Web, wrapped)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-W1** | The IDE window serves VS Code Web for the session's project and current computer, so that only API and data traffic crosses the network on repeat loads. | Static workbench assets are cached; a cold open on a slow link renders the shell promptly and degrades gracefully. |
| **FR-W2** | The wrapping layer applies CSS/DOM overrides for a native-feeling titlebar and a responsive layout for tablet and phone widths, on an unmodified VS Code Web build. | Implemented today by `proxy/` (the iframe wrapper, overlay, keyboard policy). A CI check diffs the served bundle against its pinned release. |
| **FR-W3** | Native window chrome is suppressed where the injected titlebar replaces it, without losing window controls. | Verified per platform. |
| **FR-W4** | Two runtimes: OSE (DarkPyonix-built from MIT source, Open VSX) by default, and VSC (the user's installed Microsoft build, Microsoft Marketplace) as an option. *[provisional — from `docs/design/INTEGRATION.md`]* | Choosing VSC shows an install notice, a copyable install guide, and a command field to verify `code --version` before `code serve-web` is used. |
| **FR-W5** | On Android and iOS the IDE window works without Node: through a `serve-web`-compatible Rust backend, or directly through web APIs with no backend. [user] | Opens and edits a project on a phone with no Node installed anywhere on the device. Only web-capable extensions run in this mode (`INTENT.md` D10). |
| **FR-W6** | vscode-darkpyonix (notebook renderer) and vscode-darkpyonix-theme are installed by default. [user] | Present on first launch for both runtimes. |
| **NFR-W1** | Extensions are tested unmodified from the marketplace matching the runtime. | Regressions block release. |

---

## §B — Native bridge (IDE window webview ↔ native shell)

Unchanged in substance from 09-22; see `ARCHITECTURE.md` §3 and `INTENT.md` D11.

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-B1** | Injected script detects a tab drag-out gesture without interfering with VS Code's own tab reorder. | A short drag reorders; a long drag detaches; never both. |
| **FR-B2** | On detach, the webview posts file URI, cursor, scroll, selection and source window to the native host through the platform's webview message API. | Versioned schema; mismatch is logged, not dropped. |
| **FR-B3** | The native host opens a new IDE window at the same project and computer with that state. | The window appears within one frame of the gesture completing. |
| **FR-B4** | The bridge is bidirectional (native → webview pushes). | Additive fields need no version bump. |
| **NFR-B1** | New IDE window open-to-usable ≤ a plain `serve-web` page load + 100 ms. | p99 on the reference machine. |
| **NFR-B2** | One bridge message ≤ 5 ms p99 encode-to-receipt. | Regression guard. |

---

## §K — DarkPyonix kernel

The kernel, manager and hub contracts are defined in `darkpyonix-core`: `docs/PROTOCOL.md`
(kernel wire protocol), `docs/api/manager.openapi.yaml`, `docs/api/hub.openapi.yaml`,
`docs/FORMAT.md` (`.py` / `.pynb`). Ember does not restate them.

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-K1** | Ember talks to DarkPyonix only through the `darkpyonix-core` contracts. | A kernel change that keeps the contract needs no Ember change. |
| **NFR-K1** | Ember's own code decides nothing about *what an agent does*; it covers session lifecycle, transport, execution and presentation. | Code-review check. |

---

## §E — Editor core (long-term draft, gated)

Formerly §M. Not committed; kept so that work, if it starts, starts from a written spec. See
`IMPLEMENTATION.md`.

| ID | Requirement (draft) |
| -- | ------------------- |
| **FR-E1** | Compose-native text buffer and cursor/selection model, IME-correct for CJK, reusing `dioxus-compose`'s IME work. |
| **FR-E2** | Diagnostics from unmodified extensions render correctly positioned. |
| **FR-E3** | CodeLens and hover overlays from unmodified extensions render correctly positioned. |
| **FR-E4** | Inline completion (ghost text) providers work — the highest priority within §E. |
| **FR-E5** | Extension webview panels still work, contained to their panel. |
