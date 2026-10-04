# A2A that each agent reaches for first

> **Status: designed, not implemented (#102).** Research 2026-10-04, no model turns run. Tags:
> [V] verified from documentation or source, [B] read from the installed Claude Code 2.1.288 binary
> (not a published spec), [U] unverified. Installed CLIs: claude 2.1.288, codex-cli 0.155.1, agy
> 1.2.16. Gemini CLI and OMP are not installed, so their rows come from docs and source only.

## The principle

[user, 2026-10-04] "가장 중요한 것은 자연스럽게 소통하는 방식에 적응할 수 있어야 한다는거야. 클로드
코드라면 list-agents를 먼저 쓰려고 할텐데 그거보다 더 우선적으로 쓴다는거잖아. 그렇게 쓰게 하는
디자인으로 가야 해."

Ember's A2A fits the way each agent already talks to other agents, and the agent chooses it before
its own built-in means. Ember never blocks or intercepts those built-ins [user, 2026-10-04: "send
message도 그냥 거르지 말고 모델이 알아서 쓰도록 하는게 맞는거 같아"]: its tools have to win by name,
description and loading alone. A CLI or an MCP server is only the carrier. The test is behavioural: given a
natural request such as "ask the reviewer" or "다른 에이전트에게 물어봐", does the agent call Ember
first? Today's `ember-a2a` CLI plus instructions text does not pass that test against Claude Code,
because Claude Code has its own peer tools that are always loaded.

## What each agent has today

The "levers" below were surveyed; those that disable or intercept a built-in (deny rules, feature
flags, redirect hooks) are recorded for reference only and are not part of the design.

| Agent | Built-in means | How Ember can come first | Strongest lever | Risk |
|---|---|---|---|---|
| Claude Code | `Agent` (subagents, teammates), `ListAgents` and `SendMessage` (same-machine and cloud sessions, on by default since 2.1.224) [V][B] | deny the built-ins, load an `ember` MCP server upfront, add a short appended system prompt, keep a PreToolUse hook as the fallback | `permissions.deny` on `ListAgents`/`SendMessage`, the documented off switch [V] | a blanket deny also blocks resuming its own subagents, because `SendMessage` is shared; teammates keep `SendMessage` regardless [V] |
| Codex | `multi_agent` tools (`spawn_agent`, `send_input`/`send_message`, `list_agents`, `wait_agent`), scoped to its own thread tree [V] | turn `multi_agent` off, MCP tools with the v2 argument shapes, an AGENTS.md paragraph, a PreToolUse redirect [V] | the feature flag | removes Codex's own subagents (E2), so it becomes a setting |
| Gemini CLI | subagents exposed as tools named after the subagent, `@name` [V] | `excludeTools`, a BeforeTool deny, `mcp_ember_*` tools, GEMINI.md text [V] | hooks | live behaviour [U] |
| Antigravity | `define_subagent`, `invoke_subagent`, `send_message`, `manage_inbox` [B] | MCP plus a `hooks.json` PreToolUse deny with a pointer reason (hooks tested locally, SPEC §A) | hooks | model-visible descriptions and MCP ranking [U] |
| OMP | `task` subagents; peers write to `agent://<name>` [V] | expose Ember sessions as `agent://` names if an extension loads under ACP, otherwise MCP with the same wording [U] | the `agent://` scheme | extension loading [U] |

AionUI (iOfficeAI/AionCore) solves the same contest with 13 `team_`-prefixed MCP tools and one
injected line: "Your platform may provide similarly named built-in tools. Do NOT use those." A
distinctive prefix avoids name collisions, MCP is primary and a CLI is the fallback. It publishes no
measurement of how often it wins. [V]

## Two directions for Claude Code

**(A) Join Claude Code's peer bus**, so Ember sessions appear in `ListAgents` and the native
`SendMessage` becomes Ember A2A. Same-machine delivery uses per-session Unix sockets
(`/tmp/cc-socks/<pid>.sock`); only the own-session posting path is documented
(`CLAUDE_CODE_MESSAGING_SOCKET`, `CLAUDE_CODE_MESSAGING_TOKEN`) [V]. The peer wire format and the
roster record are not published [U], the feature changed across five releases [V], and no terms
clause on third-party use was found [U]. It would reach Claude Code only; Codex, Antigravity and
OMP still need (B). Not pursued unless the protocol is documented and the terms question is
answered.

**(B) Same-shaped Ember tools first.** An `ember` MCP server with `alwaysLoad`, tools
`list_agents` and `send_message` using the native argument names (`to`, `message`), descriptions
that open with the user's verbs ("ask, consult, tell, list the other agents and sessions, on any
Ember computer and any model"), and the server's instructions saying that these reach every Ember
session while the built-ins reach only the agent's own. The built-ins stay enabled; nothing is
denied and no hook redirects a call. Supported surfaces only. This is the direction for every
agent.

## Keeping the runtime token out of the shell

Today the token is exported to the agent process as `EMBER_RUNTIME_TOKEN`
(`ember/server/src/a2a/mod.rs`), so `env` or a prompt injection can read it. Options:

| Option | Gain | Cost |
|---|---|---|
| Ember-launched stdio MCP server holding the token in its own environment | the agent's Bash does not see it; fits (B) | the launch config is readable; keep it in a 0600 file outside the workspace and deny that path |
| Unix socket per session with peer credentials | no secret at all | all agents share one uid; pid ancestry mapping is racy |
| Per-call token injected by a hook | short lived, never shown to the model | needs a bootstrap secret; hook contracts differ per agent |
| Shell prefix wrapper | Claude only | touches every Bash call |

Draft: the MCP server holds a session-bound, A2A-scoped, expiring token, and the server checks the
caller's pid as a second factor. The shell CLI stays for people and debugging, with its weaker
exposure stated.

## Measuring "reached for first"

A fixture Ember server runs three live sessions (Claude, Codex, Antigravity), each holding a
distinctive fact, and records every A2A call. Each trial starts a fresh agent and captures its
ordered tool calls (Claude and Antigravity stream-json, `codex exec --json`, ACP events).

- Prompts: at least 12 English and Korean paraphrases ("consult the reviewer", "get a second
  opinion from another model", "tell payments the schema changed", "which agents are running",
  "다른 에이전트에게 물어봐"), plus negative controls ("spawn a helper to search the repo").
- N: 30 per prompt per condition; 50 for a go or no-go decision; record model and CLI versions.
- Built-ins stay enabled in every condition. Conditions, cumulative: no Ember; CLI and
  instructions (today); MCP deferred; MCP always loaded; plus appended prompt.
- Metrics: first-choice Ember rate among A2A attempts, native calls at any position (counted as
  failures, never blocked), detours
  (tool search, `--help`) before the first Ember call, delivery confirmed by the recorder, wrong
  targets, false positives on negatives.
- Leakage detectors: native tool names per agent, shell access to `cc-socks`, and any send the
  model claims that the recorder did not see.
- Re-run on every CLI version bump.

Targets [user, 2026-10-04: "초안대로 확정하면 되긴 할거같은데"]: at least 95% Ember-first with
the built-ins enabled, at most 2% false positives, re-measured whenever an agent CLI version
changes.

## Completion and idle reports (FR-T8)

[user, 2026-10-04: "AionUI는 리더한테 각자 작업이 끝났고 한거가 전부 다 보고가 들어가더라고 idle로
들어가면 리더가 바로 확인 없이 알게되는 방식인거 같던데 그런 인터렉션이 필요할거같아."]

A session doing delegated work (an A2A request, a team task, a spawn) reports back on its own, so
the delegator never polls. Claude Code does this with `SendMessage`'s `notify_when_idle` and with
subagent hand-backs; AionUI's teammates report to the lead when they go idle.

- **When:** the turn ends and the session is idle; an error ends the turn; an approval or a
  question waits on a person; no progress for a configurable stall time.
- **What:** the outcome (done, failed, needs input, stalled), a summary of the last response, files
  changed and PRs or commits created in the turn, the error for a failure, and a link to the full
  transcript.
- **To whom:** the team leader for a team task, otherwise the sender of the message the session
  was working on. A session with no delegator sends nothing.
- **Noise:** one report per real turn end (deduplicated by session and turn, with a short settle
  window for automatic follow-ups); at most one stall report per stall; reports count toward loop
  protection and never start a reply turn on their own.
- **How it arrives:** as one A2A message in the receiver's own idiom (FR-T2).

## Sources

code.claude.com docs (cross-session messaging, tools reference, hooks, MCP, subagents, agent
teams, settings, environment variables); the claude 2.1.288 binary; openai/codex
`codex-rs/core/src/tools/handlers/multi_agents_spec.rs` and the hooks schema; iOfficeAI/AionCore
`crates/aionui-api-types/src/team_tools.rs` and `crates/aionui-team-prompts`; can1357/oh-my-pi
`docs/agent-hub.md` and `prompts/`; gemini-cli docs (subagents, MCP server, hooks reference); local
Antigravity hook tests (SPEC §A).
