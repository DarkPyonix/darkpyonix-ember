# PROJECT.md — DarkPyonix Ember

> **Revised 2026-10-03.** Scope follows `docs/INTENT.md` (conversation-first, one main server,
> sessions that move between computers). `docs/BACKGROUND.md` records how the 09-22 VS Code design
> was reached; it still governs the IDE window.

## Scope

Ember is a multi-provider, LLM-based development environment and remote IDE. Its job is to make
four things work together:

1. **A main server** — a personal Raspberry Pi or Mac mini — that runs every wrapped agent CLI
   (Claude Code, Codex, Antigravity, OMP), stores every conversation, and manages every account.
2. **Computers** assigned to projects, each running a thin execution daemon, so that an agent on
   the main server can work on whichever computer the session is currently using — and move.
3. **A native client** — launcher and conversation screens on `dioxus-compose`, no webview — that
   puts conversations first and opens an IDE only when needed.
4. **An IDE window** — VS Code Web, wrapped (`proxy/`), or an external IDE (VS Code, JetBrains
   Gateway) — launched from a conversation.

Kernel, manager and hub APIs are `darkpyonix-core`'s; Ember links to them.

## Method

Spec first, then a failing test, then code. An ID exists in `docs/SPEC.md` before behaviour
lands. A decision that changes scope goes into `docs/INTENT.md` first, with what it rejected.

Ember-specific additions:
- Every decision in `INTENT.md` and `SPEC.md` is tagged **[user]** or **[provisional]**. A
  provisional item may be built against, but not treated as settled; when the user confirms or
  overturns it, the tag changes in its own commit.
- Each wrapped agent is integrated against **that agent's actual, current behaviour** (its CLI
  flags, its non-interactive protocol, its session files), verified by an integration test, never
  by assumption. If an agent's real behaviour and these documents disagree, the documents are
  corrected.
- VS Code questions are still tracked in `docs/IMPLEMENTATION.md` against upstream architectural
  facts.

## Milestones

*[provisional — ordering proposed by the implementer, not yet confirmed by the user]*

| ID | Milestone | Decides |
| -- | --------- | ------- |
| **M0** | These documents, revised to the 10-03 brief and cross-referenced | Whether the conversation-first, main-server model has a coherent written spec |
| **M1** | Main server core: Claude Code and Codex wrapped (`FR-A1`–`FR-A4`), transcripts stored on the server (`FR-S1`–`FR-S3`), sessions surviving client disconnects, a push channel (`PR-1`). Executes on the main server itself only. | Whether headless wrapping keeps each agent's native behaviour (E2) well enough to be the foundation |
| **M2** | Execution daemon on a second computer (`FR-X1`, `FR-X2`, `FR-X5`) and switching a session between computers (`FR-X3`, `FR-S7` v0) | Whether "one AI moving between computers" works end to end, and what invalidation it really needs (Q1, Q4) |
| **M3** | Native client on `dioxus-compose`: projects, sessions with status, computers, conversation view, "Open IDE" (`FR-L1`–`FR-L9`) | Whether the conversation-first client meets `dioxus-compose`'s performance bar with Ember's data model; depends on `dioxus-compose`'s own readiness |
| **M4** | A2A (`FR-T1`–`FR-T6`) and accounts with usage routing (`FR-U1`–`FR-U3`, `FR-U5`) | Whether cross-vendor, cross-account messaging is useful without being a loop hazard |
| **M5** | Networking: peer-to-peer with `darkpyonix.dev` hole punching and relay (`FR-N1`–`FR-N4`) | Which transport (Rust tunnel or an existing mesh — Q7); also resolves HTTPS for phones |
| **M6** | Remote browser with server-held profile, and agent browser use (`FR-R1`–`FR-R5`) | How the browser reaches the client without breaking E1 |
| **M7** | IDE window: `proxy/` integrated as the wrapping layer, OSE/VSC runtimes, bridge (`FR-W1`–`FR-W6`, `FR-B1`–`FR-B4`); mobile without Node (`FR-W5`) | Whether the wrapped IDE holds up from a conversation on desktop and phone |
| **M8** | Compose-native editor core (§E) — **not committed**, gated behind M1–M7 and real usage data | See `IMPLEMENTATION.md` |

`proxy/` already delivers much of M7's wrapping layer and a transitional multi-machine home; it
keeps working throughout and is folded in rather than rewritten (`INTENT.md` D13).

## Open questions

The full list with status is in `docs/INTENT.md` → *Open questions*. Summary:

| ID | Question |
| -- | -------- |
| Q1 | Are a project's computers copies of one workspace, machines with different roles, or "whichever computer I am at"? |
| Q2 | Who moves a session between computers: the agent, the user, or both? |
| Q3 | Does an open IDE window follow a session when it moves? |
| Q4 | How are a transcript's computer-specific observations invalidated on a move? |
| Q5 | What happens to a job left running on the previous computer? |
| Q6 | Where exactly is each agent intercepted so its native behaviour is untouched? |
| Q7 | Which peer-to-peer transport ("tailcat" — Tailscale?) |
| Q8 | Is the Tauri scaffold kept long-term, and for what? |
| Q9 | `proxy/`'s carried-over items: login, pre-distribution security holes |
| Q10 | (from 09-22, still open) What is VS Code Web's renderer ↔ extension host wire protocol, and how stable is it? Only matters for M8. |
| Q11 | What is "OMP" in the agent list, and how does it run headless? |

## Rejected alternatives (summary — see `INTENT.md`)

- **Each computer runs its own agents and keeps its own transcripts**, with a hub aggregating
  them. This is what `proxy/`'s hub does today, and it is the arrangement that makes moving a
  conversation between computers painful. Replaced by the main server (D3, D13).
- **Patching or reimplementing the agents** to add Ember features. Violates E2; Ember adds around
  agents only.
- **Agents coordinating through a shared file.** The status quo that A2A replaces (D5).
- **Forking VS Code** or **reimplementing the Extension Host.** Unchanged from 09-22 (E4, D11).
