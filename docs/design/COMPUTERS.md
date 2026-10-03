# COMPUTERS.md: computers, switching and tool interception (#6)

> Status: **implemented, not yet run end to end** (2026-10-03). Covers SPEC `FR-X2`, `FR-X3` and
> `FR-S7` v0 on ember server. The mechanism choice is in `INTERCEPTION.md`; this note records
> what was built, what it relies on, and what is still open. **[V]** = verified locally (evidence
> named), **[U]** = unverified.

## What exists

| Piece | Where | What it does |
|-|-|-|
| Registry | `server/src/computers/mod.rs` (`Registry`) | Tables `computers(id, name, url, token, created_at)` and `session_computer(session_id, computer_id, env_json, notice, switched_at)` in `ember.db`, created by store migration 3 (`computers/schema.rs`) and read through the main `Store` connection, like `accounts`. |
| Service | `computers::Computers` | Register / list / probe (`/v1/health`, `/v1/env`) / remove; a session's current computer; `switch`. Installs three hooks on `Sessions`: an instructions hook (environment block), a start-config hook (Codex remote executor / Claude Code shell shim), and a message hook (the FR-S7 notice). |
| HTTP API | `server/src/computers/api.rs` | `GET/POST /api/v1/computers`, `GET/DELETE /api/v1/computers/{id}`, `GET/PUT /api/v1/sessions/{id}/computer`. |
| Codex relay | `server/src/computers/relay.rs` | Loopback WebSocket (`ws://127.0.0.1:<port>/<secret>`) that codex app-server connects to; re-frames to the node's raw `/v1/exec-server` stream. |
| Node bridge | `node/src/exec_server.rs` | `GET /v1/exec-server` (bearer auth) runs `codex exec-server --listen stdio` per connection and relays bytes; `ember-node exec-server` runs the same command on its own stdio. |
| Browser egress | `server/src/computers/egress.rs`, `node/src/egress.rs` | Per computer, a loopback SOCKS5 listener (`socks5://127.0.0.1:<port>`) that a project's browser uses as `--proxy-server`; each connection becomes one node `/v1/egress` stream where the node runs SOCKS5 (FR-R1, see `REMOTE-BROWSER.md`). A computer that is some project's browser egress cannot be removed (409). |
| Claude shim | `server/src/computers/shim.rs`, `server/src/bin/ember-exec.rs` | `CLAUDE_CODE_SHELL_PREFIX` target; runs Bash-tool commands on the node via `/v1/exec` and carries the cwd back. |
| Mount | `server/src/computers/mount.rs` | **Design and stub only** (`NoMount`). |

The local server is the implicit computer `local`. A session with no `session_computer` row
behaves exactly as before this change: no environment block, nothing redirected.

## Switching (FR-X3, FR-S7 v0)

`PUT /api/v1/sessions/{id}/computer {computer_id}`:

1. Refused with 409 while the session is mid-turn (`running` or `waiting_for_approval`), 404 for
   an unknown computer or session, 502 when the target fails `/v1/health` (or speaks another node
   protocol version) or `/v1/env`.
2. Records the computer and its `/v1/env` snapshot.
3. If the session's agent has already run (it has a native id), queues a notice: the computer
   changed and earlier file contents, listings and command output must be re-read / re-run.
4. Releases the agent process. The next message starts it again (native resume) with:
   - the **environment block** built from `/v1/env` as system-level instructions — Claude Code
     `--append-system-prompt=…`, Codex `developerInstructions` on `thread/start`/`thread/resume`.
     It is contributed by an instructions hook (`Sessions::add_instructions_hook`), joined with
     the other hooks' instructions (A2A, …). Because hooks run at every process start, a switch
     **replaces** it; it is never appended.
   - the per-agent redirection below.
5. The next `send` records `SystemNotice{text}` (distinct from `Notice`, which the agent never
   sees) then `UserMessage{text}` (separate events), and
   the agent receives `"<notice>\n\n<message>"`. The notice is taken once (persisted until then,
   so a server restart does not lose it).

Switching to the computer a session is already on is a no-op (`changed: false`).

## Per agent

**Codex — tools run on the node (shell, unified exec, PTY, apply_patch file ops [U]).**
The start request carries `RemoteExec {environment_id: "ember-<computer id>", exec_server_url}`.
The adapter then sends `initialize` with `capabilities: {experimentalApi: true,
requestAttestation: false}`, `environment/add {environmentId, execServerUrl}`, `thread/start`
with `environments: [{environmentId, cwd}]`, and `environments` again on every `turn/start`
(`thread/resume` has no such field). The app-server process starts in the temp directory when
the project path does not exist on the server.

**Claude Code — Bash runs on the node; file tools are local-only.**
Environment: `CLAUDE_CODE_SHELL_PREFIX=<abs path of ember-exec>`, `EMBER_EXEC_NODE_URL`,
`EMBER_EXEC_NODE_TOKEN`, `EMBER_EXEC_REMOTE_SHELL` (the node's `$SHELL`). Read / Edit / Write /
Glob / Grep still operate on the server's disk until the mount lands, so the project path must
exist on the server (the adapter refuses to start otherwise). Hooks and stdio MCP launches, which
the prefix also wraps, run locally by default (`EMBER_EXEC_NON_TOOL=remote` sends them too).

## Protocol facts relied on

| Fact | Status |
|-|-|
| `environment/add {environmentId, execServerUrl, connectTimeoutMs?}` → `{}`; also `environment/info`, `environment/status`; notifications `thread/environment/connected|disconnected` | [V] `codex app-server generate-ts --experimental` (0.155.1) |
| `ThreadStartParams.environments` and `TurnStartParams.environments: TurnEnvironmentParams[] = {environmentId, cwd, runtimeWorkspaceRoots?}`; absent from the non-experimental output; `ThreadResumeParams` has none | [V] same, diffed against `generate-ts` without `--experimental` |
| `InitializeCapabilities {experimentalApi, requestAttestation, …}` opts into the experimental API | [V] same |
| `developerInstructions` on `thread/start` and `thread/resume` | [V] same; whether it replaces or merges with config's instructions on resume is [U] |
| `codex exec-server --listen stdio`; it exits when stdin closes. `--exit-on-stdin-close` requires `--environment-id`/`--remote` (remote registration only) | [V] `codex exec-server --help` + manual run, and an end-to-end Codex turn on a node |
| exec-server stdio framing is newline-delimited JSON; its WebSocket form is one message per Text frame | [U] (the relay assumes both) |
| codex keeps the URL path (`/<secret>`) when connecting to `execServerUrl` | [U] |
| Claude Code builds `… && eval <cmd> && pwd -P >| <cwd-file>` and, with the prefix set, runs `$SHELL -c -l "<quoted prefix> <quoted command>"` (prefix split at its last ` -` so trailing flags stay flags) | [V] `claude` 2.1.288 binary strings (functions building the exec command and joining the prefix) |
| The same prefix wraps hook commands and stdio MCP launches (`prefix 'cmd args'`) | [V] same |

## Open problems

- **Not run end to end.** No real Codex turn through a remote environment and no real Claude
  Bash call through `ember-exec` has been made yet; verify both on the Pi.
- **Claude file tools** stay local until `mount.rs` is implemented; the node API also lacks
  rename/remove/mkdir/symlink/chmod and a change feed (`INTERCEPTION.md` recommendation 4).
- **Claude cwd tracking**: the shim writes the node's `pwd -P` to the local cwd file; if that
  directory does not exist on the server, Claude Code may reset its cwd. The mount fixes this.
- **Token exposure**: the node token sits in the Claude process environment (and so in its local
  hooks' environment). A per-session scoped token is the fix.
- **Codex needs a codex binary on the node** (outside FR-X5's "ember node only"), and the
  exec-server is not confined by the node's path policy.
- **Node protocol**: `/v1/exec-server` and `/v1/egress` were added without bumping `PROTOCOL_VERSION` (additive).
- Codex `additionalContext` (keyed context on `turn/start`) may be a cleaner carrier for the
  environment block than `developerInstructions`; not tried.
- Switch is refused mid-turn; whether a user should be able to force it is open (`INTENT.md` Q2).
