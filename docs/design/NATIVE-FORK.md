# Native session fork per agent (FR-S5, #78)

Verified on 2026-10-25 against the installed CLIs and their published schemas. The server forks
natively where the agent can and copies the conversation otherwise (SPEC FR-S5, INTENT D16).

| Agent | Native fork | From a chosen turn | How Ember calls it |
|---|---|---|---|
| Codex | yes | yes | app-server `thread/fork {threadId, lastTurnId}`; `lastTurnId` is "the last turn id to fork through, inclusive" |
| Claude Code | yes | last turn only | `claude --resume=<id> --fork-session` (new session id; init event announces it) |
| ACP agents | if advertised | last turn only | `session/fork {sessionId, cwd, mcpServers}` when `agentCapabilities.sessionCapabilities.fork` is present |
| Antigravity | no | no | none: `agy --help` lists only `--continue` and `--conversation <id>`; Ember copies the conversation |

## Sources

- Codex: `codex app-server generate-json-schema` (the installed CLI), `v2/ThreadForkParams.json`
  (`threadId`, `lastTurnId`, `cwd`, `model`, `sandbox`, `approvalPolicy`, `developerInstructions`,
  `excludeTurns`, `ephemeral`) and `v2/ThreadTurnsListParams.json` (`threadId`, `limit`,
  `sortDirection`). There is no rollback request in the v2 schema. `codex fork [SESSION_ID]` also
  exists as a CLI command (`codex fork --help`).
- Claude Code: `claude --help`: `--fork-session` "When resuming, create a new session ID instead of
  reusing the original (use with --resume or --continue)". `--resume-session-at <message id>` is
  hidden from the main help and described only as a print-mode truncating resume. Using it needs
  the uuid of a message from the session file, which Ember does not store, so Ember forks the whole
  session and only from the last turn.
- ACP: the unstable schema (`schema/v1/schema.unstable.json`, agentclientprotocol/agent-client-protocol
  main): `ForkSessionRequest {sessionId, cwd, additionalDirectories, mcpServers}`,
  `ForkSessionResponse {sessionId, modes, configOptions, ...}`, `SessionForkCapabilities`. Marked
  UNSTABLE. It takes no turn.
- Antigravity: `agy --help` (flags listed above). No fork flag or subcommand.

## Not verified

No real fork was run (no agent session was started for this change). The Claude Code and ACP calls
are written from the documented flags and schema; the first real run is the check.
