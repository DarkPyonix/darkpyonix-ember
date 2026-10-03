# SPEC.md: DarkPyonix Ember

Functional (`FR-*`), non-functional (`NFR-*`) and protocol (`PR-*`) requirements. Every
implementation traces to an ID here; a requirement with no test is not done.

> **Revised 2026-10-03** to follow `INTENT.md`'s conversation-first, main-server model. The 09-22
> launcher section (§L) is rewritten. The IDE window (§W), its bridge (§B), the kernel (§K) and
> the editor-core draft (§E, formerly §M) are kept with small changes.
>
> Tags: **[user]**: from the user's brief. **[provisional]**: team proposal, not confirmed by
> the user; may be overturned. Requirements without a tag follow directly from a `[user]` decision
> in `INTENT.md`.

Areas: **L** launcher and conversation UI · **S** sessions and transcripts · **A** agent wrapping ·
**X** execution on computers · **T** agent-to-agent (A2A) · **R** remote and agent browser ·
**U** accounts and usage · **N** networking · **W** IDE window · **B** bridge · **K** kernel ·
**E** editor core (draft).

---

## Status (2026-10-04)

What exists on `develop`, checked against the code, CI and the issues. **CI** means the crate is
in `.github/workflows/checks.yml`'s `cargo test` matrix (server, node, transport, hub, client,
bridge, editor-conn, editor), which passes on `develop`. `crates/app` (`ember-app`, the native UI)
is not in that matrix, because it pulls `dioxus-compose` as a git dependency; the session builds it.
Live agent tests (`EMBER_E2E_*`) and anything that needs a second computer, a real network, a
screen or a real hub are run by hand and are listed as not verified until they are.

| IDs | Status | Issue |
| --- | ------ | ----- |
| FR-L1–L9 | Implemented: client core (`crates/client`, CI) and screens (`crates/app`, not in CI). Server side of FR-L4 and FR-L9 in CI (#49). Not verified on a display. | #9 |
| NFR-L1 | Not measured. | #9 |
| NFR-L2 | `scripts/check-no-webview.sh` runs in CI. | |
| FR-S1–S4, FR-S6 | Implemented, CI. | |
| FR-S5 | Not implemented: `can_fork: false`, `POST …/fork` answers 501. | none |
| FR-S7 | v0 (system notice) implemented, CI (#34). Per-file hash comparison planned. | #6 (v0); target: none |
| FR-A1–A4 | Claude Code and Codex (#14), Antigravity (#64), ACP agents (#61) implemented, CI against recordings and fakes. Real OMP run not verified. agy shell writes fail under `--sandbox`. | #53, #65 |
| FR-A5 | Allow once, always, deny and the pending list implemented; Antigravity hook FR-A5a–f implemented (#64). The approve-everything mode is not implemented, in the server or the client. | #73 |
| FR-A6–A8 | Implemented, CI (#14, #58). | |
| FR-X1–X3 | Implemented, CI (#20, #34, #47, #48); verified end to end with a node on the same Mac. A switch between two physical computers is not verified; the project mount (`mount-nfs`, `mount-fuse` features) is not built in CI. | #6 |
| FR-X4 | Implemented in ember node (`jobs.rs`), CI. | |
| FR-X5, NFR-X1 | Not measured. | none |
| FR-T1–T7 | Implemented, CI (#18, #59). Teams of real agents on different computers not verified. | none |
| FR-R1–R4 | Implemented on ember server and node, CI (#29, #44). Acceptance runs (egress IP, login across an egress switch, takeover) not verified. | #12 |
| FR-R5 | Not implemented: no client renders the browser. | #12 |
| FR-U1–U3, FR-U5 | Implemented, CI (#19). | |
| FR-U4 | Implemented, CI (#38); not run against OpenAI. | #16 |
| FR-N1, FR-N3, FR-N5 | Implemented, CI against the in-memory transport (#23, #45). Two-NAT run not done. | #10 |
| FR-N2 | Implemented, CI against `FakeHub` (#50, #57); not run against the real hub. The default hub address and unversioned hub paths are designed, not merged: draft PR #75, waiting for darkpyonix #41 and darkpyonix-core #42. | #62 |
| FR-N4 | Not verified on a real phone. | #10 |
| NFR-N1 | Loopback bench only; the network matrix is not measured. | #10 |
| PR-1 | Implemented with `PUSH_VERSION` 1. Dropping the `v` field (D15) is planned. | #63 |
| FR-W1–W3 | `web/proxy/` (#1). FR-W3 not verified per platform. | none |
| FR-W4 | OSE builds in CI (#24, #39); the manual Pi and Mac step is not recorded. | none |
| FR-W5, FR-W5a–h, NFR-W5a | Designed only (`docs/design/MOBILE-NO-NODE.md`), outside the 10-18 deadline. | none |
| FR-W6 | Extensions are in `extensions/` (#69); default install and the notebook criteria are open. | #15 |
| NFR-W1 | No release process yet. | #72 |
| FR-P1–P6 | ember node sessions, CI (#37, #42); VS Code companion in `web/proxy/companion`. The Ember editor's terminal panel is not built; acceptance runs not verified. | #26 |
| NFR-P1 | Not measured. | #26 |
| FR-B1–B4 | `detach.js` and `crates/bridge` (CI) implemented (#31). No platform webview host; not verified in a native shell. | #30 |
| NFR-B1, NFR-B2 | Not measured. | #30 |
| FR-K1, NFR-K1 | Rules for review, no code. | |
| FR-E1–E5 | Connection layer (`crates/editor-conn`, #33, #35, #40) and session layer (`crates/editor`, #46), CI. Not wired into the client's "Open IDE → Ember"; the widget bridge and an unmodified extension on screen are not verified. | #32 |

Cross-cutting, designed and not landed: REST paths `/api/v1` → `/api` (D15, #63); releases and
installers (#72); the documents in the house writing style (#68).

---

## §L: Launcher and conversation UI (dioxus-compose, no webview: E1)

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

## §S: Sessions and transcripts (main server: E3)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-S1** | A session belongs to a project and has a mutable *current computer* attribute (`INTENT.md` D1). | Changing the current computer does not change the session's identity, history, account or A2A address. |
| **FR-S2** | Every session's transcript is stored on the main server in one normalised representation, regardless of which agent produced it. Each agent's own native session state (e.g. Claude Code's session file, Codex's thread) is also kept on the main server, so the agent's own resume works. | Restarting the main server loses no completed turn. Resuming a session after a restart uses the agent's native resume (`--resume <id>` for Claude Code, thread resume for Codex). |
| **FR-S3** | A session survives every client disconnecting, and any number of clients can attach to it at once. | Close all clients mid-turn; the turn completes; a later client sees the full result. Two clients attached at once both see live output. |
| **FR-S4** | Full-text search across all sessions' messages. | Indexed search (not a linear scan); results link to the matching message. |
| **FR-S5** | A session can be forked from any completed turn where the agent supports it. | The fork records its parent and turn; agents that cannot fork have the action disabled, not failing. |
| **FR-S6** | Idle sessions release their agent process and reconnect transparently on the next message; a session being viewed is kept alive. | Measured: an idle session's agent process exits after the idle timeout; sending a message restores it via native resume. A session open in a client is not reclaimed. |
| **FR-S7** | *[provisional, `INTENT.md` Q4]* When a session's current computer changes, observations of the previous computer are invalidated. | v0: a system notice tells the agent the computer changed and prior file observations must be re-read before editing. Target: per-file content-hash comparison so only changed files are flagged. |

### §S status: server APIs behind the launcher (FR-L4, FR-L9, FR-S4, FR-S5)

- **Projects** are a table (`projects`, store migration 8), filled from existing sessions on
  migration and on every session creation. **Computer assignment** (FR-L4) is many-to-many
  (`project_computers`): `GET/POST /api/v1/projects`, `PUT`/`DELETE
  /api/v1/projects/{name}/computers/{computer_id}` (`local` = the main server). Removing a
  computer removes its assignments.
- **Session metadata** (FR-L9): `pinned` and `archived` columns (migration 9) and the existing
  title, changed with `PATCH /api/v1/sessions/{id}` `{title?, pinned?, archived?}`. Not activity:
  `updated_at` is unchanged.
- **Push** gains `session_updated` and `project_updated` (additive; `PUSH_VERSION` stays 1;
  clients skip unknown types), so a second client sees renames, pins, archives and assignments
  without reloading.
- **Search** (FR-S4): an FTS5 index (`messages_fts`, `unicode61` tokenizer) of user and
  assistant messages, kept current by a trigger on `events` and backfilled on migration.
  `GET /api/v1/search?q=&limit=` answers `{session_id, seq, kind, snippet, title, project,
  archived}`, best match first; each word matches as a prefix (Korean `오류` finds `오류가`).
  The bundled SQLite (`libsqlite3-sys` with rusqlite's `bundled` feature) is compiled with
  `SQLITE_ENABLE_FTS5`.
- **Export** (FR-L9): `GET /api/v1/sessions/{id}/export`, `format: "ember-transcript"`, the
  record and every stored event.
- **Paths**: the routes above carry `/api/v1`; D15 moves them to `/api` in one change (#63,
  planned).
- **Fork** (FR-S5): not implemented for any agent (no issue yet). `GET /sessions/{id}` reports `can_fork:
  false` and `POST /sessions/{id}/fork` answers 501 with a reason. Codex's `thread/fork` is the
  likely first implementation.

---

## §A: Agent wrapping (E2: native behaviour preserved)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-A1** | Claude Code, Codex, Antigravity and OMP (oh-my-pi) run on the main server as their own unmodified CLIs. [user] | Each agent's version is the vendor's release; no agent binary or package is patched. |
| **FR-A2** | Each agent is driven through its own non-interactive protocol, chosen per agent: Claude Code via `--print --input-format stream-json --output-format stream-json` with `--permission-prompt-tool stdio`; Codex via `codex app-server` JSON-RPC; Antigravity via its stream-json print mode, one process per turn; other agents (including OMP) via the Agent Client Protocol (ACP). *[provisional]* ACP is also a candidate for Claude Code, Codex and Gemini through their ACP adapters; the per-agent choice compares native-behaviour preservation (E2) between the vendor's own headless protocol and its ACP adapter (`INTENT.md` Q6). | An integration test per agent runs a turn that reads a file, edits it and runs a command, and verifies the normalised events (`FR-A3`). |
| **FR-A3** | All agents' output is normalised into one event model (message, tool call, tool result, approval request, usage, turn end) driving one session state machine. | Adding an agent requires an adapter only; the UI, storage and A2A need no change. Matches `web/proxy/dpx/agents/`'s adapter rule. |
| **FR-A4** | The agent's own settings, models, modes, session IDs and resume keep working as they do natively. | A session started in Ember can be resumed with the agent's own CLI on the main server, and vice versa where the agent supports it. |
| **FR-A5** | Tool approvals: allow once, always allow (for this session and tool kind), deny; a list of pending approvals; an opt-in mode that approves everything. | For agents with no headless approval channel (Antigravity), approval is obtained through the agent's own hook mechanism (a pre-tool-use hook calling back to the main server), not by patching the agent. |
| **FR-A6** | Supported agents are detected automatically on the main server, with their installed versions. | Detection runs at startup and on demand; a missing agent is shown as not installed, not as an error. |
| **FR-A7** | The main server manages MCP servers centrally and injects them into each agent session at creation. | An MCP server added once is visible to every agent that supports MCP. |
| **FR-A8** | Scheduled tasks: cron expressions (with time zone), fixed intervals, and one-off runs; each run either continues an existing session or starts a new one. Agents may create schedules from within a conversation. | Missed triggers while the server was down are detected and reported, not silently dropped. |

> The 09-22 transcript parsers in `web/proxy/dpx/agents/` (Claude Code, Codex) satisfy part of
> `FR-A3` for reading existing history and are kept (`INTENT.md` D13).

> **ACP adapter (`FR-A2`, #53).** `crates/server/src/agents/acp.rs` is one generic ACP client (protocol
> version 1, schema `schema-v1.24.1`): `initialize`, `session/new`, native resume through
> `session/resume` (or `session/load` with the replay dropped), `session/prompt`, `session/cancel`;
> `session/update` mapped to `FR-A3` events; `session/request_permission` mapped to `FR-A5`
> approvals by option kind (`allow_once` / `allow_always` / `reject_once`); the client methods
> `fs/read_text_file`, `fs/write_text_file` and `terminal/*` served on the main server, or on the
> session's computer through its node API (`FR-X1`). ACP agents are configured, not compiled in
> (`EMBER_ACP_AGENTS`, preset `omp` = `omp acp`), and appear as agent kinds by their configured
> name. *[provisional]* Covered by tests against an in-process fake ACP agent; the `FR-A2`
> acceptance test against real OMP (`crates/server/tests/acp_omp.rs`, `EMBER_E2E_OMP=1`) has not been
> run (OMP is not installed on the main server). Gaps: no account isolation (`FR-U2`) or
> transcript import for ACP agents; instructions reach agents without an instructions flag in
> front of the first prompt of each process.

### §A Antigravity: the reinforced hook (FR-A5, `INTENT.md` D14) [user]

Antigravity (`agy`) has no headless approval channel and no ACP. Ember runs it with
`--dangerously-skip-permissions --sandbox` and makes its own `PreToolUse` hook (matcher `*`, in a
per-session folder passed with `--add-dir`) the only gate. **[user, 2026-10-03]** the "reinforced
hook" design, relayed by the darkpyonix leader. Implementation: `crates/server/src/agents/antigravity.rs`.

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-A5a** | The hook config sets an explicit `timeout` (3600 s); Ember answers `deny` before it expires (route 3480 s < hook curl 3540 s < agy 3600 s) and shows "approval timed out; retry". If agy reports another timeout, Ember denies at 20 s. | Unit tests `unanswered_approval_times_out_as_deny_with_a_retry_notice`, `recorded_hooks_listing`; test 3 below. |
| **FR-A5b** | The hook prints exactly `allow` or a `deny` and exits 0 on any error (curl missing, unreachable server, non-2xx, unknown or revoked token, empty or unexpected body). It never answers `ask`. | `hook_script_*` unit tests, `a_hook_call_after_the_run_ended_is_denied_by_the_hook`; tests 4. |
| **FR-A5c** | A fresh random token per run, revoked when it ends. Ending, interrupting or shutting down a turn denies every pending approval and resolves its card; hook calls outside a turn are denied. | `broker_waits_for_the_answer`, `turn_end_denies_pending_cards_and_later_hooks`. |
| **FR-A5d** | Before **every** turn `agy -p /hooks --output-format json` must list Ember's hook from this session's folder, enabled, `PreToolUse` matcher `*`; otherwise the turn is refused. | `a_hook_that_was_not_loaded_fails_the_turn`, `recorded_hooks_listing`. |
| **FR-A5e** | `--sandbox` is passed (off only with `EMBER_AGY_SANDBOX=0`). The agy version is pinned (`PINNED_VERSION` = 1.2.16); another version gets a notice and runs read-only: no `--dangerously-skip-permissions`, only read-only tools inside the workspaces, everything else denied without asking. | `only_the_pinned_version_is_gated`, `read_only_policy_*`, `another_agy_version_runs_read_only`. |
| **FR-A5f** | No global agy configuration is written: the hook lives in the session folder, so the user's own agy sessions are unaffected. Tool calls naming the session folder are denied (it holds the token and the hook). Read-only tools are free only inside the workspaces and agy's `brain/`; elsewhere they ask. | `the_session_root_is_off_limits`, `reads_are_free_only_inside_the_workspaces`. |

**Hook tests on agy 1.2.16** (2026-10-03, macOS, model `gemini-3.6-flash-low`; standalone
`sh` hook logging its stdin; throwaway folders under `.scratch/agy-tests/`; harmless commands
only). Every run: `agy -p "<prompt>" --output-format stream-json --add-dir <folder>/root
--new-project --model gemini-3.6-flash-low --dangerously-skip-permissions [extra]` from
`<folder>/work`, `root/.agents/hooks.json` = one `PreToolUse` hook, matcher `*`. The prompt asked for
`touch marker` unless noted; "marker" = whether the file appeared anywhere in the folder.
Recordings: `crates/server/tests/fixtures/agy/*_1.2.16.*`.

| # | Test | Setup | Observed | Result |
| - | ---- | ----- | -------- | ------ |
| 0 | Hook loaded from an untrusted `--add-dir` folder | `agy -p /hooks --output-format json --add-dir root` | JSON `command.data.hooks[]` with `name`, `enabled`, `source` (the file), `actions[].event/matcher/timeout_seconds` (3600) | PASS |
| 1 | Hook runs under skip-permissions | hook answers `allow`, timeout 3600 | hook called once (payload logged), `touch` ran: marker created (in the session folder: the model chose it as `Cwd`) | PASS |
| 2 | `deny` blocks | hook `deny`; also with `--sandbox` | step `ERROR`, `tool call denied by pre-tool hook: …`; no marker, both runs | PASS |
| 3 | Long timeout holds the tool | (a) timeout 600, hook sleeps 90 s then `deny`; (b) no timeout (default 30), sleeps 45 s then `deny`; (c) timeout 600, sleeps 70 s then `allow` | (a) tool waited 90.3 s, denied, no marker; (b) hook killed at 31 s, `failed: command failed: signal: killed`, **no marker** (AionCore saw 1.1.9 run the tool here); (c) tool ran after 70.2 s | PASS |
| 4 | Hook failures | exit 1, no output; exit 2 with `allow` on stdout; `kill -9` itself; prints `this is not json {`; command `/nonexistent/…` | all: step `ERROR` (`exit status 1` / `exit status 2` / `signal: killed` / `failed to unmarshal result` / `exit status 127`), no marker | PASS |
| 4+ | Other outputs (extra probes) | exit 0 with: empty stdout (blank line, or zero bytes); `{}`; `{"decision":""}`; `{"decision":"maybe"}`; `{"decision":"ask"}` | **empty stdout → tool RAN**; **`ask` → tool RAN** (skip-permissions approves it); `{}`, `""`, `maybe` → denied | FAIL-OPEN, compensated: Ember's hook never prints empty or `ask` (FR-A5b) |
| 5 | Sub-agents, `call_mcp_tool`, browser | (s) hook denies only `run_command`, prompt delegates `touch` to `invoke_subagent`; (m) hook `deny`, stdio test MCP server in `root/.agents/mcp_config.json`; (b) hook `deny`, `open_browser_url file://…` | (s) the sub-agent's `run_command` reached the hook (its own `conversationId`), denied, no marker; (m) MCP server got `initialize`/`tools/list`, `call_mcp_tool {ServerName, ToolName}` reached the hook, denied, server never got `tools/call`; (b) model reported the browser tools unavailable (no browser connected), no tool call | PASS (s, m); browser **untestable** here |
| 6 | Several hooks: deny wins | `a-allow`+`b-deny`; `a-deny`+`b-allow`; `allow` and `deny` hooks in two `--add-dir` folders | denied in all three; hooks run in order, `allow` does not stop later hooks, `deny` does | PASS |
| 7 | Parallel tool calls | one response with three `run_command` (`touch m1/m2/m3`), hook sleeps 5 s then `deny` | three hook calls (steps 2, 3, 4), run one after another, all denied, no markers | PASS |

Other observations (1.2.16): the hook inherits agy's environment; the session folder is listed
first in `workspacePaths` even when the project is also passed with `--add-dir`, and the model
picked it as `Cwd` (FR-A5f denies that); `--sandbox` refused shell writes even in the project
(`touch: marker: Operation not permitted`) while `write_to_file` worked; the stream now carries
`tool_info.output`, `tool_info.error.message` and `text_delta`; `--new-project` left one project
file per run in `~/.gemini/config/projects/` (not used by the adapter; the test files were
removed). Not verified: browser tools; the read-only fallback against a real different agy
version; the end-to-end adapter test (`EMBER_E2E_AGY=1`) on 1.2.16. Open: shell writes under
`--sandbox` (#65).

**Outcome:** tests 1–7 pass on 1.2.16 (browser tools untestable); Antigravity sessions on 1.2.16
run gated, any other version read-only.

---

## §X: Execution on computers (D4)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-X1** | Each computer runs one **ember node** (execution daemon) that carries out tool actions for sessions (file read/write, directory listing, search, command execution with PTY) and streams results back to the main server. | A CLI on the main server, wrapped, performs a read-edit-run cycle whose effects appear on the target computer's disk and processes only. |
| **FR-X2** | The wrapping is transparent to the agent: paths, working directory and environment the agent sees are the target computer's. | The agent's own "print working directory" and environment queries report the target computer's values. |
| **FR-X3** | A session's current computer can be switched. *[Who triggers it is open: `INTENT.md` Q2.]* | After a switch, the next tool action runs on the new computer; the environment description given to the agent is replaced, not appended (`FR-S7`). |
| **FR-X4** | *[provisional, `INTENT.md` Q5]* A background job started on one computer keeps running after the session switches away, and its completion is reported into the session. | Start a long build on A, switch to B, finish the build on A: the session receives the result. |
| **FR-X5** | ember node is the only Ember component required on a computer for agent work; it does not run agent CLIs or store transcripts. | Measured: daemon RSS stays bounded and independent of the number of sessions using that computer. |
| **NFR-X1** | *[provisional]* Before and after moving agents to the main server, measure whether local slowdown came from agent runtimes and transcripts (memory) or from builds and tests (CPU). | If CPU dominates, add a concurrency limit per computer on the main server. |

---

## §T: Agent-to-agent messaging (D5)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-T1** | Any session can list other sessions it may message and send a message to one, regardless of agent vendor, computer or account. | A Claude Code session and a Codex session on different computers and accounts exchange a message and a reply. |
| **FR-T2** | Agents use A2A through a small CLI or MCP tool available inside every session, authenticated by a per-session runtime token. | No agent needs modification; the tool is injected (as a skill or MCP server) at session creation. |
| **FR-T3** | A delivered message enters the target session through the same path as a user message, with a header naming the sender session, its project/computer and a reply reference. | The target agent can reply with one call using the reply reference. |
| **FR-T4** | Messages to a sleeping (idle-released) session are queued durably and wake it. | Restart the main server with a message queued; it is delivered after restart. |
| **FR-T5** | Loop protection: per-session send rate and per-pair rate limits within a time window. | Two agents instructed to reply to each other forever are stopped by the limit, with a visible notice in both sessions. |
| **FR-T6** | Users can mention another session from the composer, and can turn A2A off per user or per session. | Off means sends to and from that session are rejected with a clear reason. |
| **FR-T7** | A leader session can spawn teammate sessions, assign tasks, and read a shared task list and mailbox. | Tasks and mailbox are stored on the main server; each teammate keeps its own approvals. |

### §T status: teams and mentions (FR-T6, FR-T7, issue #55)

- **Teams** *[provisional]* (store migration 11: `teams`, `team_members`, `team_tasks`,
  `team_mail`). The first `ember-a2a team spawn` makes the calling session the leader of a new
  team. A teammate is a new session in the leader's project and directory with the agent,
  account and computer the leader chose (defaults: the leader's agent, the account router, the
  main server) and nothing else: no model, no transcript, no approvals. Each teammate answers its
  own approvals in its own session. Its first prompt arrives as an A2A message from the leader
  (FR-T3), and its instructions name its role. At most 8 active teammates; spawns count against
  the per-session limit of the window. A session belongs to at most one team, as leader or
  teammate, for good; no nested teams. Ending a teammate (leader, or the user) stops its agent
  and removes it from the team; the session and transcript stay.
- **Permissions** *[provisional]*: only the leader spawns and ends. Any member adds tasks; the
  leader assigns and updates any task; a teammate updates only tasks that are unassigned or its
  own and assigns only to itself. An assignment to someone else, and a teammate's status change
  (to the leader), are announced through the A2A queue under loop protection, best effort.
- **Mailbox**: `mail send <name>|--all` stores the mail and delivers it through the A2A queue
  (wakes the recipient, survives a restart, FR-T4); loop protection counts one message per
  recipient (FR-T5), all or nothing. The leader reads all team mail; a teammate reads team-wide
  mail and its own.
- **Messaging rule** *[provisional]*: an active teammate is reachable only from its own team
  (leader and teammates) and reaches only its own team; it is left out of everyone else's
  `ember-a2a list` and mention candidates. Leaders and ended teammates are ordinary sessions.
- **Mentions** *[provisional]*: the composer writes `@@<id>`, `@@<title>` or `@@"<title>"`. The
  user's message goes to the session it was typed in, unchanged; each mentioned session gets a
  copy as an A2A message from that session, so it can answer the origin with one call. Chosen
  over redirecting the message because the user typed in this conversation and expects it to
  continue here, and an unresolved mention then loses nothing. Mentions follow the switches and
  the messaging rule but not the rate limits (a person sent them); they are stored, so they
  count toward the window for later agent sends. Each outcome is a notice in the origin session.
- **APIs**: agent routes under `/api/v1/a2a/team` (members, tasks, mail; runtime token = the
  caller); user routes `GET /api/v1/sessions/{id}/team`, `GET /api/v1/teams/{id}`,
  `GET /api/v1/teams/{id}/mail`, `POST /api/v1/teams/{id}/members/{session}/end`,
  `GET /api/v1/sessions/{id}/mentions?q=`. Every team change is pushed as `team_updated` with the
  whole team (additive; `PUSH_VERSION` stays 1). Details: `crates/server/src/a2a/api.rs`.
- **Not yet verified**: a team of real Claude Code and Codex sessions on different computers and
  accounts (needs real agents and hardware); the composer suggestion list on screen.

---

## §R: Remote browser and agent browser use (D6)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-R1** | A browser session can be opened whose network egress is a chosen computer. The egress path is a SOCKS5 exit in that computer's ember node, carried over a transport stream (`FR-N5`), which the main server's browser uses as its proxy. | A what-is-my-IP page shows the chosen computer's public IP; a page on that computer's LAN or `localhost` is reachable. |
| **FR-R2** | Browser profile data (cookies, storage, logins, history) is stored on the main server and reused whichever computer is the egress. | Log in to a site with egress A, switch egress to B, reload: still logged in. |
| **FR-R3** | Agents can drive the same browser through a browser-automation tool (DevTools-protocol based), and the user can see and take over the page the agent is driving. | An agent fills a form while the user watches; the user takes over to complete a login; the agent continues afterwards. An "agent is active" indicator is shown. |
| **FR-R4** | Browser data can be cleared per project. | Clearing removes cookies and storage for that profile only. |
| **FR-R5** | The remote browser is available from every client, including mobile and web, not only from a desktop app. | Verified from the phone client. |

> How the browser is rendered to the client without breaking E1 (pixel streaming into a native
> surface, or confining it to a separate window that is allowed a webview) is open; it must not put a
> webview in the launcher or conversation screens.

---

## §U: Accounts and usage (D7)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-U1** | The main server holds several accounts per agent (Claude Code, Codex, Antigravity), each with isolated credentials and configuration. | Two Claude Code accounts run sessions simultaneously without sharing credentials or settings. |
| **FR-U2** | A new session is started under a chosen account. | The session's account is shown in the conversation view and cannot silently change. |
| **FR-U3** | Usage per account is recorded and visible; usage routing can choose the account for a new session by policy (e.g. least used, failover when one is rate-limited). | A rate-limited account is skipped by the router and the reason is shown. |
| **FR-U4** | Sign in with OpenAI ("Sign in with ChatGPT", OAuth 2.0 / OIDC with PKCE and a loopback redirect), with a page that lets ChatGPT plan usage be consumed in addition to Codex token usage. [user] | Works for a **self-hosted** ember server only: OpenAI permits open-source, locally hosted apps to call the Responses API on the user's ChatGPT Plus/Pro plan, with a per-app weekly cap, `store:false` and `stream:true` required, and no image generation, file search, code interpreter or hosted MCP. It is never offered through `darkpyonix.dev` (remote hosting needs OpenAI's approval) [user, 2026-10-03: "OpenAI 로그인은 엠버 서버에서 사용자가 자체적으로 하는걸로 하고 허브는 깃허브 로그인으로 하자"]. Source: developers.openai.com/siwc/token-sharing-open-source, per the darkpyonix leader's research, 2026-10-03; endpoints, dynamic client registration (`dynamic_agent_client`) and limits verified against the sub-pages the same day (see `docs/design/CHATGPT-SIGNIN.md`). Off when `EMBER_HOSTED=1`. |
| **FR-U5** | API-key providers (any OpenAI-compatible or vendor API) can be added with keys encrypted at rest. | Keys never appear in transcripts, logs or exports. |

---

## §N: Networking (D8)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-N1** | Main server ↔ computer and client ↔ main server connections are HTTPS carried over a peer-to-peer tunnel. **Transport: iroh 1.0, decided, conditionally** [user, 2026-10-03: "P2P를 iroh로 가는건 일단 허용하는데 그게 품질이 별로면 아예 직접 구현하는거도 고민해봐"]. In-process Rust (one app on mobile), QUIC hole punching, self-hostable relay, MIT/Apache-2.0. If it misses `NFR-N1`, our own implementation is evaluated. Lives behind `FR-N5`. | Works across two different NATs with no port forwarding configured by the user. |
| **FR-N2** | `darkpyonix.dev` provides hole-punching coordination and a relay fallback: `darkpyonix.dev` runs iroh-relay and an address directory (decided with the transport); devices register under the user's **GitHub account** on the hub [user, 2026-10-03: "허브는 깃허브 로그인으로 하자"], so connections need no user configuration. | A new computer joins by signing in; no address or port is entered by hand. |
| **FR-N3** | All connections are encrypted and authenticated per device; a device can be revoked. | Revoking a device closes its connections within one heartbeat. |
| **FR-N4** | Clients reach the IDE window over a secure context, so VS Code Web's service-worker-backed webviews work on phones and tablets. | Extension webviews render on a real phone (not only headless Chromium; see `web/proxy/docs/CONSTRAINTS.md`). |
| **FR-N5** | All Ember code reaches the network through one transport interface (connect to a peer by its key, accept, open bidirectional streams, report path state: direct or relayed). **No iroh type appears outside the transport crate.** | Replacing the transport touches only that crate: a CI check fails if `iroh` is imported anywhere else, and the ember server and ember node test suites run unchanged against an in-memory fake transport. |
| **NFR-N1** | Transport quality bar. **If iroh misses any line after tuning, our own implementation is evaluated** (`FR-N1`). Initial targets, set before measurement; the first M5 measurement may adjust a target once, with the measured data and reason recorded here. | Measured on the real network matrix: home router ↔ school/office network, home ↔ LTE hotspot, and symmetric NAT on one side; 20 runs per pair. <br>• **Direct-path success:** ≥ 85% across the matrix excluding symmetric-NAT pairs; symmetric-NAT pairs must still connect via relay 100%. <br>• **Direct-path overhead:** RTT ≤ raw path + 5 ms (p50) and + 15 ms (p95); throughput ≥ 80% of a raw TCP transfer over the same path. <br>• **Connection setup:** first byte ≤ 1.5 s p95 (relay allowed); direct path established ≤ 5 s p95 when one exists. <br>• **Relay → direct upgrade:** ≤ 10 s p95 after a direct path becomes possible; direct → relay fallback with no stream reset. <br>• **Network change** (Wi-Fi ↔ LTE): open streams survive; stall ≤ 3 s p95. <br>• **Mobile:** an idle background connection adds ≤ 2%/hour battery drain (Android and iOS); reconnect on foreground ≤ 1 s p95. |
| **PR-1** | Main server → client push channel for session status, transcript updates, computer reachability and assignments. | Versioned schema; a version mismatch is detected and reported, not silently dropped. |


### §N status: transport wiring (M5, issue #10)

Where the acceptance criteria stand after wiring `ember-transport` into the real connections.
The tests named below run in CI against the in-memory transport and `FakeHub`; nothing here has
run on real networks (#10) or against the real hub (#62).

| ID | What is wired | Evidence / what remains |
| -- | ------------- | ----------------------- |
| **FR-N1** | ember node serves its API on transport service `ember-node/1` (`EMBER_NODE_TRANSPORT=1`, or `only`); ember server dials nodes registered by peer (`POST /api/v1/computers {name, peer, token}`) and serves its own API on `ember-server/1` (`EMBER_TRANSPORT=1`); the client crate reaches the server with `Api::over_transport`. One transport stream = one HTTP/1.1 connection, so every HTTP route and WebSocket (exec, events, exec-server, terminal attach, push) is unchanged. | `crates/node/tests/transport.rs`, `crates/server/tests/transport.rs`, `crates/client/tests/transport_flow.rs` (fake transport). The two-NAT acceptance run is still to do on real networks (`NFR-N1` matrix). |
| **FR-N2** | Relay URL from `EMBER_RELAY_URL`; peers addressed by `PeerAddr` (id + hints) or bare id. **Hub** (`ember-hub` crate, `docs/design/HUB-INTEGRATION.md`, against `hub.openapi.yaml` v0.3.0): ember server (`POST /api/v1/hub/link`, token sealed with `secret.key`) and ember node (`ember-node hub register`, `<state dir>/hub.json` 0600) join the user's GitHub account through a device link (user code + verification URL, polled claim signed with the endpoint key). A registered endpoint publishes its signed address record to the hub's `/pkarr` and resolves peers there with its token (`ember_transport::HubDirectory`: iroh's own pkarr publisher/resolver inside the transport), and uses the hub's relay (`EMBER_HUB_URL`, default `https://darkpyonix.dev` → `https://relay.darkpyonix.dev`). Computers are added by picking a device from the account (`POST /api/v1/hub/devices/{id}/computer`); the devices allow-list can sync from the account (opt-in); removal on the hub is detected (`401` on `/v1/me`) and surfaced. | Tests run in CI: `crates/hub/tests/{link_flow,revocation,directory}.rs`, `crates/server/tests/hub.rs`, `crates/node/tests/hub.rs` against `ember_hub::fake::FakeHub`. Hub-side gaps (relay URL discovery, revocation reason, client role, self-removal…) are listed in `HUB-INTEGRATION.md` §Spec gaps. Until a node is registered, pasting its `PeerAddr` still works. Planned (#62, draft PR #75, waiting for darkpyonix #41 and darkpyonix-core #42): the default hub moves off the root domain and the hub paths lose `/v1`. |
| **FR-N3** | Per-device allow-lists enforced at accept (`ember_transport::PeerGate`): the server admits peers in its `devices` table (store migration 6; `/api/v1/devices`, served on TCP only), the node admits server peer ids from `EMBER_NODE_ALLOWED_PEERS` / `<state dir>/allowed-peers`. Revoking (`DELETE /api/v1/devices/{peer}`; node: edit the file + SIGHUP) closes the peer's open connections immediately, which is within one heartbeat. Bearer tokens stay as a second factor for now. | `crates/transport/tests/gate.rs`; revocation cases in the three suites above. |
| **FR-N5** | All of the above uses `ember-transport`'s API only. | `scripts/check-transport-isolation.sh` passes; server, node and client tests run against `MemNetwork`. |

---

## §W: IDE window (VS Code Web, wrapped)

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-W1** | The IDE window serves VS Code Web for the session's project and current computer, so that only API and data traffic crosses the network on repeat loads. | Static workbench assets are cached; a cold open on a slow link renders the shell promptly and degrades gracefully. |
| **FR-W2** | The wrapping layer applies CSS/DOM overrides for a native-feeling titlebar and a responsive layout for tablet and phone widths, on an unmodified VS Code Web build. | Implemented today by `web/proxy/` (the iframe wrapper, overlay, keyboard policy). A CI check diffs the served bundle against its pinned release. |
| **FR-W3** | Native window chrome is suppressed where the injected titlebar replaces it, without losing window controls. | Verified per platform. |
| **FR-W4** | Two runtimes: OSE (DarkPyonix-built from MIT source, Open VSX) by default, and VSC (the user's installed Microsoft build, Microsoft Marketplace) as an option. *[provisional, from `docs/design/INTEGRATION.md`]* | Choosing VSC shows an install notice, a copyable install guide, and a command field to verify `code --version` before `code serve-web` is used. |
| **FR-W5** | On Android and iOS the IDE window works without Node: through a `serve-web`-compatible Rust backend, or directly through web APIs with no backend. [user] | Opens and edits a project on a phone with no Node installed anywhere on the device. Only web-capable extensions run in this mode (`INTENT.md` D10). |
| **FR-W5a** | *[provisional, `docs/design/MOBILE-NO-NODE.md`]* On Android and iOS, "Open IDE → VS Code" opens the session's project on its **current computer**: the IDE window loads that computer's own `serve-web` (OSE or VSC) through the in-app gateway (`FR-W5b`) over the transport (`FR-N5`). Node runs only on the computer. | On a phone with no Node anywhere on the device: open a project, edit and save a file, and the change is on the computer's disk. An extension with only a `main` (Node) entry installed on that computer works. An extension webview renders on a real phone (`FR-N4`). |
| **FR-W5b** | *[provisional]* One Rust **ide-gateway** in the app process serves the IDE window from a loopback origin that the platform webview treats as a secure context, with a second secure origin for `webviewEndpoint`. It reverse-proxies HTTP and WebSocket over transport streams (mode `FR-W5a`) and serves static workbench assets and the working-copy file API (mode `FR-W5f`). | Binds loopback only and rejects requests without the per-launch token. `navigator.serviceWorker` exists in both origins on iOS and Android. No `iroh` type is imported outside the transport crate (`FR-N5`). Wi-Fi ↔ LTE switch keeps the workbench connected within `NFR-N1`'s stall bound. |
| **FR-W5c** | *[provisional]* On Android and iOS, "Open IDE → Ember IDE" opens files of the current computer in the dioxus-compose editor core through ember node file operations (`FR-X1`) over the transport. No webview, no JS engine. | Open, edit and save a remote file from a phone, over a direct path and over the relay. If the file changed on the computer since it was opened, saving reports a conflict instead of overwriting. `NFR-L2`'s webview check covers the editor core. |
| **FR-W5d** | *[provisional]* A project subtree can be made available offline as a **local working copy** in the app sandbox, recording each file's base content hash; the editor core edits it with no network. | In airplane mode: open, edit, save, create and delete files in the working copy; the app restarts with the edits intact. Excluded paths and the size limit are honoured. |
| **FR-W5e** | *[provisional]* When online, working-copy changes sync to a chosen computer of the project; a file changed on both sides is never overwritten. | Edit file X offline only on the phone and file Y on both sides: X is written to the computer; Y is shown as a conflict with a three-way diff, and neither side's content is lost. |
| **FR-W5f** | *[user: `FR-W5` "directly through web APIs with no backend"; form provisional]* Offline VS Code mode: the IDE window boots a bundled, pinned OSE web build with no `remoteAuthority`, so every extension runs in VS Code's own web-worker extension host; the working copy is exposed through a built-in `ember-fs` web extension. Configured only through the embedder API (`IWorkbenchConstructionOptions`); VS Code source is not patched (E4). | In airplane mode on Android: open the working copy, edit, save, and run a web extension (a theme, a grammar, and one language extension with a `browser` entry). The served bundle matches its pinned release (`FR-W2`'s diff check). No Node on the device. |
| **FR-W5g** | *[provisional]* In any no-Node mode, the extensions view says for each extension whether it runs on the phone (`browser` entry) or needs a computer, and extensions without a `browser` entry are not offered for local install. | Classification comes from the extension manifest (`browser`, `main`, `extensionKind`); a `main`-only extension shows "needs a computer" and a one-tap switch to `FR-W5a`. |
| **FR-W5h** | *[provisional]* On iOS, `FR-W5f` (downloaded extension code) ships only behind a flag, enabled after the App Review decision (`MOBILE-NO-NODE.md` Q-M3). `FR-W5a`–`FR-W5e` ship without it. | An iOS build with the flag off downloads and executes no extension code locally. |
| **NFR-W5a** | *[provisional: initial targets, to be adjusted once by the first measurement]* Mobile IDE responsiveness. | On a reference mid-range Android phone and a reference iPhone: `FR-W5a` warm open to editable ≤ 3 s p95 over a direct path; `FR-W5c` remote file open ≤ 1 s p95 for a 100 KB file; `FR-W5f` cold open ≤ 5 s p95; IDE-window RSS recorded per mode. |
| **FR-W6** | vscode-darkpyonix (notebook renderer) and vscode-darkpyonix-theme are installed by default. [user] | Present on first launch for both runtimes. |
| **NFR-W1** | Extensions are tested unmodified from the marketplace matching the runtime. | Regressions block release. |

> **`FR-W5a`–`FR-W5h`, `NFR-W5a`** come from `docs/design/MOBILE-NO-NODE.md` (2026-10-03, design only;
> outside the 10-18 deadline). The phone is mainly a client of a computer's own `serve-web` (`FR-W5a`)
> or ember node (`FR-W5c`); offline it edits a local working copy (`FR-W5d`, `FR-W5e`), optionally in
> VS Code Web with web extensions only (`FR-W5f`). **Rejected:** a `serve-web`-protocol-compatible
> server on the phone: the web workbench always opens a remote extension-host connection when a
> remote exists, which E4 forbids us to answer with our own host, and with no Node every extension
> runs in the web worker anyway. Whether this reading of `FR-W5`'s wording is right is open
> (`MOBILE-NO-NODE.md` Q-M1). No issue tracks the build yet.

### FR-W4 acceptance: the OSE runtime

OSE is built by `.github/workflows/ose.yml` from `build/ose/` (Code-OSS at the tag in `build/ose/VERSION`,
`product.json` overrides only; see `build/ose/README.md`). A release (tag `ose-v*`) is accepted when,
for every target (linux-x64, linux-arm64, darwin-arm64, darwin-x64):

1. **No Microsoft marketplace or telemetry in `product.json`.** `build/ose/check-product.sh` passes on
   the packaged `product.json`: `extensionsGallery` points at Open VSX
   (`serviceUrl` `https://open-vsx.org/vscode/gallery`, `itemUrl` `https://open-vsx.org/vscode/item`,
   resource/extension templates on `open-vsx.org`); `enableTelemetry` is not true and there is no
   `aiConfig`; no `marketplace.visualstudio.com`, `*.vsassets.io`, `vscode-unpkg.net`, update,
   experiments or voice endpoint appears anywhere. Remaining Microsoft hosts (the webview CDN,
   Copilot doc links) are listed in the job log and in `build/ose/README.md`.
2. **It launches.** `build/ose/smoke.sh` starts `bin/dpx-ose-server` with the flags `web/proxy/dpx` uses
   (`DEFAULT_OSE_ARGS`: `--host --port --without-connection-token --accept-server-license-terms
   --server-data-dir`) and the workbench page is served.
3. **Open VSX search and install work.** `build/ose/smoke.sh` queries `<serviceUrl>/extensionquery` and
   gets results, and the server CLI installs an extension (`redhat.vscode-yaml` by default) from
   Open VSX.
4. **Manually, once per release, on a Raspberry Pi (64-bit OS) and a Mac:** `python -m dpx.serve`
   with `DPX_OSE_SERVER` set opens the IDE window; the Extensions view searches Open VSX and
   installs an extension; Help → About shows "DarkPyonix OSE" and the Code-OSS version.

Steps 1–3 run in CI on every build; step 1 alone runs on every pull request touching `build/ose/`.

---

## §P: Persistent work in IDE windows

[user, 2026-10-03: "엠버에서 vscode나 엠버 에디터 같은 경우 그 안에서 실행 중인 작업들은 창을 끄더라도
항상 실행되고 있어야 한다는거 잊지 마. 만약 터미널에서 뭔가를 켜놨다 하면 창 꺼도, 다른 컴퓨터에서 접속해도
그 터미널이 보여야 하는거야."] Applies to both IDE targets that Ember hosts: the VS Code window (VS Code
Web) and the Ember editor.

**Design.** The terminal process is owned by **ember node's persistent PTY session** on that computer,
not by the IDE window, VS Code's pty host, or the client. The IDE is only a client that attaches to it.
This way a window close, a VS Code server restart, or a client switching device does not end it, and
VS Code and the Ember editor see the same terminal. VS Code's own persistent terminals
(`terminal.integrated.enablePersistentSessions`) are not relied on, because a server restart or the
revive timeout ends them.
Design, attach API and VS Code settings: `docs/design/TERMINALS.md`.

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-P1** | Terminals, tasks and launched processes started from an IDE window keep running after every window on every device is closed. | In a VS Code window run `sleep 600; echo done`, close the window, and wait 10 minutes. The process is still alive on the computer (visible in ember node's session list) and prints `done`. The same applies to the Ember editor. |
| **FR-P2** | Re-attaching shows the same terminal, from the same device or from any other computer or phone. Scrollback (at least 10,000 lines) and the live input state carry over: the shell prompt, a running TUI, and cursor position. | Start `htop` or a REPL in one window. Close it. Open the IDE on another computer (or phone). The same terminal appears in the terminal list with its full scrollback and the TUI redrawn, and accepts input. |
| **FR-P3** | The same terminal can be attached from several devices and from both IDE targets at once. | A terminal opened in VS Code appears in the Ember editor's terminal list on another device. Output appears on both within 100 ms of each other on a LAN. |
| **FR-P4** | Input from several attached clients: **all attached clients may type** and their keystrokes are serialized by ember node in arrival order. Any client can **take control**, after which other clients are read-only, with a visible "controlled by <device>" banner, until control is released or the controller detaches. The terminal size follows the controller, or the most recently active client when nobody has control. Others see the content at that size, scaled to fit. | Two devices type alternately: the output contains both inputs in order and neither is lost. Device A takes control: device B's keystrokes are refused with the banner shown, and are accepted again after A releases or detaches. Resizing on the controller resizes the PTY. |
| **FR-P5** | VS Code tasks (`tasks.json`) and the Ember editor's run actions run in persistent sessions as well. Debug sessions keep their debuggee process running when the window closes; re-attaching the debugger is best-effort. | Start a long build task, close the window, reopen: the task's terminal is listed with its output and exits normally. A program started under the debugger is still running after the window closes. |
| **FR-P6** | Persistent sessions are listed per computer and per project in the client, with what started them (IDE, agent, user), and can be killed from there. Sessions survive an ember server restart. Sessions survive an ember node restart only if the processes were detached from it (best-effort; documented). | Restart ember server: every terminal is still listed and attachable. The list shows the origin and allows kill. |
| **NFR-P1** | Attach latency and overhead. | Re-attach shows the last screen in ≤ 500 ms on a LAN. Idle sessions cost ≤ 2 MB of memory each in ember node beyond the shell itself. |

**FR-P5 notes (debugging).** VS Code serves a debug adapter's `runInTerminal` itself and an extension
can neither answer it (trackers only observe; a `DebugAdapterDescriptorFactory` can only be
registered by the extension that defines the debug type), and a *launch* debuggee dies with its
adapter anyway (js-debug's watchdog, debugpy's launcher). So the VS Code companion rewrites a
`launch` with `"console": "integratedTerminal"` into a persistent session running the program with
the debugger listening on 127.0.0.1 plus an **attach** configuration: Node (`node`/`pwa-node`, runtime
`node`, via `--inspect-brk`) and Python (`debugpy`, via `debugpy.listen`). Other adapters and
consoles are unchanged and not persistent. A window opened later offers "Re-attach the debugger"
for this project's running debuggees. Details and the per-adapter table: `docs/design/TERMINALS.md`
§3c.

---

## §B: Native bridge (IDE window webview ↔ native shell)

Unchanged in substance from 09-22; see `ARCHITECTURE.md` §3 and `INTENT.md` D11.

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-B1** | Injected script detects a tab drag-out gesture without interfering with VS Code's own tab reorder. | A short drag reorders; a long drag detaches; never both. |
| **FR-B2** | On detach, the webview posts file URI, cursor, scroll, selection and source window to the native host through the platform's webview message API. | Versioned schema; mismatch is logged, not dropped. |
| **FR-B3** | The native host opens a new IDE window at the same project and computer with that state. | The window appears within one frame of the gesture completing. |
| **FR-B4** | The bridge is bidirectional (native → webview pushes). | Additive fields need no version bump. |
| **NFR-B1** | New IDE window open-to-usable ≤ a plain `serve-web` page load + 100 ms. | p99 on the reference machine. |
| **NFR-B2** | One bridge message ≤ 5 ms p99 encode-to-receipt. | Regression guard. |

**Implementation notes (10-03).** Webview side: `web/proxy/static/detach.js` (VS Code Web target;
the editor core reuses the schema later). Native side: the `crates/bridge/` crate (`ember-bridge`):
message types, the `WebviewBridge` trait, version handling and the detach → open-window step.
No platform webview implementation yet (#30). Details and field reliability: `web/proxy/README.md`
"Tab detach".

- **FR-B1** "never both" is enforced by deciding at `dragend` only when no VS Code drop target
  accepted the drop (`dropEffect === 'none'`) and the drop is > 48 px from the tab strip or
  outside the window. VS Code's own drag-out-of-window feature
  (`workbench.editor.dragToOpenWindow`) is defaulted off by the proxy; Alt-drag stays VS Code's.
  Dirty, multi-selected and resource-less tabs are not detached. Touch is out of scope until
  a touch drag gesture exists (VS Code's tab drag is HTML5 drag-and-drop).
- **FR-B2** positions are 0-based. `fileUri` is always present; `cursor`, `selection` and
  `scroll` are `null` when the dragged editor is not visible and has no saved view state.
  Additive fields: `workspace`, `screen`, `label`, `editor`, `stateSource`, `sentAtMs`.
  The payload is a JSON string on every platform.
- **FR-B3** without a native shell (plain browser) the fallback opens the same workspace URL
  with VS Code Web's `payload=[["openFile","<uri>:<line>:<col>"],["gotoLineMode","true"]]`:
  the cursor survives, the selection range and scroll do not. The native host loads the same
  URL (`OpenWindow::vscode_web_path`); carrying selection and scroll into the new window is open.
- **NFR-B2** `sentAtMs` on `tab_detach` lets the host measure encode-to-receipt.

---

## §K: DarkPyonix kernel

The kernel, manager and hub contracts are defined in `darkpyonix-core`: `docs/PROTOCOL.md`
(kernel wire protocol), `docs/api/manager.openapi.yaml`, `docs/api/hub.openapi.yaml`,
`docs/FORMAT.md` (`.py` / `.pynb`). Ember does not restate them.

| ID | Requirement | Acceptance criteria |
| -- | ----------- | ------------------- |
| **FR-K1** | Ember talks to DarkPyonix only through the `darkpyonix-core` contracts. | A kernel change that keeps the contract needs no Ember change. |
| **NFR-K1** | Ember's own code decides nothing about *what an agent does*; it covers session lifecycle, transport, execution and presentation. | Code-review check. |

---

## §E: Editor core (long-term draft, gated)

Formerly §M. Work has started (M8, #32): `crates/editor-conn` and `crates/editor` exist and their
tests run in CI; the rows stay a draft until the editor opens a project on screen. See
`IMPLEMENTATION.md`, `docs/design/EDITOR-CONNECTION.md` and `docs/design/EDITOR-SESSION.md`.

| ID | Requirement (draft) |
| -- | ------------------- |
| **FR-E1** | Compose-native text buffer and cursor/selection model, IME-correct for CJK, reusing `dioxus-compose`'s IME work. |
| **FR-E2** | Diagnostics from unmodified extensions render correctly positioned. |
| **FR-E3** | CodeLens and hover overlays from unmodified extensions render correctly positioned. |
| **FR-E4** | Inline completion (ghost text) providers work; this is the highest priority within §E. |
| **FR-E5** | Extension webview panels still work, contained to their panel. |
