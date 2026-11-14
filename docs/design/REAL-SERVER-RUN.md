# Real server run (smoke harness)

Status: **written, not yet run.** The script and this note were written without starting any
process (no build was available and the session does not start servers); the first run by the
leader is the first verification. Script: `ember/server/scripts/real-server-smoke.sh`.

The demo mode (`EMBER_DEMO_DATA=1`) shows the app with canned data and no server. This harness runs
the actual app against an actual ember server and ember node on this Mac, with real agent
sessions, so the parts the demo cannot show (sessions, transcripts, approvals, restarts, A2A,
channels, computers) can be checked end to end.

## 1. What it starts

| Process | Address | Data | Notes |
|---|---|---|---|
| `ember-server` | `127.0.0.1:18740` (`SMOKE_PORT`) | `<run>/server` (`EMBER_DATA_DIR`) | `EMBER_SCRIPTED_AGENT=1`, `EMBER_HUB_URL=off`, `EMBER_SERVER_RELAY=off`, no transport, no guest door, `TMPDIR=<run>/tmp` |
| `ember-node` | `127.0.0.1:18741` (`SMOKE_NODE_PORT`) | `<run>/node` (`EMBER_NODE_STATE_DIR`) | roots `<run>/work` only, `EMBER_NODE_KEEP_PTY=0`, random token in `<run>/secrets/node-token` |
| `ember-app` | | `<run>/app` (`EMBER_DATA_DIR`) | `EMBER_SERVER_URL` set, `EMBER_DEMO_DATA` unset, token from `<run>/app/server-token` |

`<run>` is `<repo>/.scratch/real-server-smoke/live` for `up` and `.../check` for `check`
(`SMOKE_DIR` moves both, but only to a place inside the repository, as `AGENTS.md` requires).
The ports differ from the default `8740`/`8741`, so a server the owner already runs is not touched;
a port held by any other process stops the script with exit code 4 instead of killing it.

Isolation, and why each setting is there:

- The server would otherwise use `~/.ember` and the app `~/Library/Application Support/ember`.
  Both are replaced by `EMBER_DATA_DIR` per process. The app's token lookup uses the same
  variable (`ember/client/src/token.rs`), so it never falls back to `~/.ember/server-token`.
- `TMPDIR` is the run's own folder: the server deletes stale private MCP config files from the
  temporary directory at start (`cleanup_stale_mcp_files`), and another server's files must not
  be among them.
- Hub, relay roles and transport are off so the run never registers with darkpyonix.dev or opens
  port 7443.
- The node's roots are the run's `work/` folder, not `$HOME`.

What is not isolated: Claude Code sessions use the owner's own Claude login and write their
native session files under `~/.claude/projects/` (the session folder is under `<run>/work`). That
is how a real session works and is the point of the run. Each Claude Code turn is a real model
call; the seeded sessions use `haiku` (`SMOKE_CLAUDE_MODEL`) to keep it cheap.

## 2. Binaries

The script uses existing builds and never builds. For each binary it takes, in order: the
`EMBER_SERVER_BIN` / `EMBER_NODE_BIN` / `EMBER_APP_BIN` variable, else the newest
`ember/<crate>/target/release/<bin>` across this checkout and every worktree
(`git worktree list`). `SMOKE_ALLOW_DEBUG=1` also accepts `target/debug`. It prints the paths it
chose and warns when the server and node come from different checkouts.

`ember-shim` and `ember-a2a` are found next to `ember-server` (as the server itself does). Without
`ember-shim` agents stop with the server; without `ember-a2a` a real agent cannot reply over A2A
or post to a channel. The script warns about both.

When a build is missing it exits with code 3 and prints, for one builder at a time
(`AGENTS.md`, "Sub-agents and builds"):

```sh
(cd ember/server && CARGO_BUILD_JOBS=2 cargo build --release --bins)   # ember-server, ember-shim, ember-a2a, ember-exec
(cd ember/node   && CARGO_BUILD_JOBS=2 cargo build --release)          # ember-node
(cd ember/app    && CARGO_BUILD_JOBS=2 cargo build --release)          # ember-app
```

On 2026-11-07 no release `ember-server` or `ember-node` existed in any worktree, so these builds
come first.

## 3. Commands

```sh
ember/server/scripts/real-server-smoke.sh check            # non-interactive, no model call, exit 0 or 1
ember/server/scripts/real-server-smoke.sh up               # server, node, app, seeded project; prints the steps
ember/server/scripts/real-server-smoke.sh steps            # print the manual steps again
ember/server/scripts/real-server-smoke.sh status           # processes, computers, sessions, channels
ember/server/scripts/real-server-smoke.sh app              # restart the app only
ember/server/scripts/real-server-smoke.sh server-restart   # restart the server only, keeping its data
ember/server/scripts/real-server-smoke.sh server-update    # replace the server while shims run (FR-S8, #128)
ember/server/scripts/real-server-smoke.sh down             # stop app, server, node and the session agents
ember/server/scripts/real-server-smoke.sh reset            # down, then delete the live run directory
```

Exit codes: 0 ok, 1 a check failed, 2 usage, 3 a binary is missing, 4 a port is taken.

### Secrets

The server token stays in `<run>/server/server-token` (written by the server, 0600) and two 0600
copies the script makes: a curl header file (`<run>/secrets/auth.header`, used as `-H @file`)
and the app's `<run>/app/server-token`. The node token is generated into
`<run>/secrets/node-token`; the computer registration body is built by `jq --rawfile` into a 0600
file and deleted after the call, and that call's response body is never printed. No token is put on
a command line the script runs or printed. The one exception is the node process itself, which
reads its token from `EMBER_NODE_TOKEN` in its own environment (the node has no token-file
option); that token is random per run and opens only that loopback node.

## 4. `check`: what it proves without a model

`check` starts its own server and node on `18750`/`18751` (`SMOKE_CHECK_PORT`,
`SMOKE_CHECK_NODE_PORT`) in a fresh `<repo>/.scratch/real-server-smoke/check`, drives the HTTP API
with curl and the scripted test agent (`ember/server/src/agents/scripted.rs`: one Bash tool call
that needs approval, then `echo: <text>`), stops everything and exits non-zero if any check
failed. The app is not started.

| Check | SPEC |
|---|---|
| `/api/health` answers without a token; no token and a wrong token get 401; the file's token gets 200; `server-token` is 0600 | FR-S9 |
| `/api/agents` answers; the installed agents are listed | FR-A6 |
| ember-node registers as a computer and is reachable | FR-X1 |
| A project and two scripted sessions are created and listed | FR-L1, FR-S1, FR-L2 |
| A message runs a turn that stops at `waiting_for_approval`; the approval request, user message, assistant reply and `turn_ended completed` are stored with consecutive sequence numbers | FR-S2, FR-A5 |
| `@@beta` in alpha's message reaches beta as an `[ember a2a]` message | FR-T6, FR-T3 |
| A channel with both sessions; a post with `@beta` reaches beta, a post without a mention reaches no agent; both are in the channel log | FR-T10, FR-T12 |
| After a server restart both sessions and alpha's whole transcript are there, and alpha runs a new turn | FR-S2, FR-S3 |
| Three `claude-code` sessions running under `ember-shim` survive stopping and updating/restarting the server mid-turn: all three agent PIDs stay alive while the server is down, the new server rejoins all three shims at startup, and all 30 ticks per session complete in order without loss (`EMBER_SERVER_UPDATE_BIN` optionally points at the updated server binary) | FR-S8 (#128) |
| The project is assigned to the node computer | FR-L4 |

`check` cannot prove: push over the WebSocket (the app shows it), a real model's streaming, an
agent's own A2A reply or `channel_post` (they need an agent process holding a runtime token), and
anything on screen.

## 5. `up`: the manual run

`up` starts the three processes, then seeds (once per run directory) the project `smoke-real`
with `alpha` and `beta` (Claude Code, model `haiku`), `gamma` (scripted, no model call), the
channel `smoke-room` holding all three, and the computer `smoke-node`. It then prints these
steps:

1. **Session appears.** The app shows no demo data. Project `smoke-real` lists alpha, beta, gamma.
   Create one more session from the app (Claude Code, model haiku) and see it in the app and in
   `status`.
2. **Transcript streams.** In alpha: `Count from 1 to 30, one number per line, then say DONE.`
   The reply grows while it is written.
3. **Approval.** In alpha: `Run the shell command date and tell me what it printed.` A Bash
   approval card appears; allow once, and the result follows. Ask again and deny. In gamma any
   message gives an approval card without a model call.
4. **Survives an app restart and server update.** Run `real-server-smoke.sh app`; reopen alpha and
   find the whole transcript and the right status. Optionally `real-server-smoke.sh server-restart`,
   then ask alpha `What did I ask you first?` (native resume). While a streaming turn is running in
   alpha and beta, run `real-server-smoke.sh server-update` (optionally with
   `EMBER_SERVER_UPDATE_BIN` set): it verifies each shim and agent PID stays alive while the server
   is down and that the new server rejoins the active turns (`FR-S8`, #128).
5. **A2A between two sessions.** In alpha: `@@beta Reply to alpha with the single word PONG using
   ember-a2a, then stop.` beta gets an `[ember a2a]` message from alpha and alpha gets the reply.
6. **Channel post.** In `smoke-room`: `@beta say hello in this channel with channel_post`. Only beta
   receives it, and its reply appears in the channel attributed to beta. A post without a mention
   reaches no agent.
7. **Computer.** The computers screen shows `smoke-node` as reachable.

`down` stops the app, the server, the session shims and their agents (found by the shims'
`--dir <run>/server/shims`) and the node, and keeps the data, so `up` again continues the same
run. `reset` deletes it.

## 6. Troubleshooting

- Logs: `<run>/logs/server.log`, `node.log`, `app.log`.
- `port N is held by pid P`: something else listens there. Set `SMOKE_PORT` / `SMOKE_NODE_PORT`.
- The app shows demo data: `EMBER_DEMO_DATA` is unset by the script, so the app binary may be an
  old build; check the path it printed.
- A Claude Code session fails at its first message: run `claude --version` and check the login in
  a terminal; `EMBER_CLAUDE_BIN` picks another binary.
