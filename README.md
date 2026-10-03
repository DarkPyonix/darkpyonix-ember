# darkpyonix-ember

**One AI, many computers.** Ember is a multi-provider, LLM-based development environment and
remote IDE. Your coding agents — Claude Code, Codex, Antigravity, OMP — run on one main server
that keeps every conversation, and do their work on whichever of your computers the task needs,
moving between them as they go.

*Ember is the client and the agent environment. DarkPyonix, underneath, is the Python kernel and
notebook runtime (`darkpyonix-core`).*

---

## 🎯 Why this exists

Three frictions from day-to-day agentic development:

- **Conversations are stuck on the computer that started them.** A CLI agent writes its transcript
  to local disk; moving work to another computer means abandoning the conversation. Ember keeps
  all conversations on **one main server** — a Raspberry Pi or Mac mini — so a conversation is no
  longer tied to a computer.
- **Agents from different vendors can't talk.** Claude and Codex can only coordinate through a
  shared file, and even two Claude sessions can't talk across computers or accounts. Ember gives
  agents **a direct channel to each other**, across models, computers and accounts.
- **A remote computer's view of the network is hard to borrow.** Ember opens **a browser that
  egresses from the chosen computer**, with its IP, while cookies and logins stay on the main
  server — one browser identity, many vantage points. Agents can drive it too.

A project is a company; its computers are branch offices. The agent works for the company, and
goes to whichever office the work needs.

---

## 🧭 The shape of the thing

1. Open Ember. The main screen lists **projects**; each shows its **conversation sessions** and
   whether they are running, waiting for approval, or finished. **Computers** are listed at the
   bottom, with which projects they serve.
2. Open a conversation. The agent is running on the main server; its tools act on the session's
   current computer. Send messages, approve tool calls, switch the computer, or let it talk to
   another agent.
3. Need to look at code? **"Open IDE"** (top right) launches Ember's IDE window — VS Code Web,
   wrapped — or VS Code, or JetBrains Gateway, on that project and computer.
4. Close the client. The sessions keep running on the main server. Open it again on a phone or
   another computer and pick up where you left off.

The UX deliberately follows **JetBrains Gateway**: a light front door, heavier sessions opened on
demand.

---

## 🏗 Architecture, one paragraph

A **main server** runs **ember server**: it runs the agent CLIs headless, stores transcripts, accounts and browser profiles,
and brokers agent-to-agent messages. Each **computer** runs **ember node**, a thin execution daemon that performs
tool actions (files, commands, browser egress) for whichever sessions are using it. The **client**
— launcher and conversation screens on `dioxus-compose`, with no webview — talks to the main
server; the **IDE window** is VS Code Web wrapped by `proxy/`. Everything connects peer to peer,
with `darkpyonix.dev` coordinating hole punching. See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

---

## 📦 Components

| Component | What it is |
| --------- | ---------- |
| **ember** | The multiplatform client and agent environment described above. Its IDE window uses VS Code's `serve-web`, or — on Android and iOS, without Node — a `serve-web`-compatible Rust backend or direct web-API access. |
| **vscode-darkpyonix** | VS Code extension rendering DarkPyonix notebooks (`.py`, `.pynb`). Installed by default. |
| **vscode-darkpyonix-theme** | VS Code theme in the DarkPyonix Ember (phoenix) design language. Installed by default. |
| **intellij-darkpyonix** | IntelliJ / PyCharm plugin rendering DarkPyonix notebooks. Installed by default. |

---

## 🚦 Status

Design stage, with one working piece.

| Layer | State |
| ----- | ----- |
| Main server (agent wrapping, sessions, accounts, A2A) | Specified (`docs/SPEC.md` §S, §A, §T, §U), not implemented |
| ember node (execution daemon) and computer switching | Specified (§X), not implemented |
| Native client (dioxus-compose) | Specified (§L); depends on `dioxus-compose` |
| Networking (P2P, `darkpyonix.dev` relay) | Specified (§N); transport not chosen |
| Remote and agent browser | Specified (§R), not implemented |
| IDE window wrapping layer | **Working** in [`proxy/`](proxy/README.md) — VS Code Web on tablets and phones |
| Compose-native editor core | Long-term, not committed (§E) |

---

## ⚖️ Non-negotiables

| ID | Constraint |
| -- | ---------- |
| **E1** | The launcher and conversation screens never contain a webview. |
| **E2** | Wrapped agents keep their native behaviour; Ember adds around them, never patches them. |
| **E3** | Conversations, agent processes and account credentials live on the main server. |
| **E4** | VS Code is wrapped, never modified; the Extension Host is never reimplemented. |
| **E5** | Inside the IDE window, a webview is scoped to the smallest region that needs it (long-term). |
| **E6** | A Compose-native editor core, if built, never diverges from VS Code's behaviour. |

Reasons, sources and the decisions behind them: [`docs/INTENT.md`](docs/INTENT.md).

---

## 🔗 Related repositories

- **[dioxus-compose](https://github.com/DarkPyonix/dioxus-compose)** — the native GUI stack the
  client is built on. Its non-negotiables apply unchanged to Ember's client.
- **[darkpyonix-core](https://github.com/DarkPyonix/darkpyonix)** (GitHub: `DarkPyonix/darkpyonix`) — the DarkPyonix kernel, manager and hub, with their API contracts
  (`docs/PROTOCOL.md`, `docs/api/`, `docs/FORMAT.md`). Ember links to these rather than redefining
  them.

---

## 🗂 Project layout

```
darkpyonix-ember/
├─ README.md            this file
├─ PROJECT.md           scope, method, milestones, open questions
├─ docs/
│  ├─ INTENT.md          motivation, non-negotiables, decisions, open questions
│  ├─ SPEC.md            FR-* / NFR-* / PR-* with acceptance criteria
│  ├─ ARCHITECTURE.md    topology: main server, computers, client, IDE window
│  ├─ IMPLEMENTATION.md  the IDE window's VS Code analysis (what can and cannot be replaced)
│  ├─ BACKGROUND.md      how the 09-22 VS Code design was reached
│  └─ design/            design material (INTEGRATION.md, decks)
├─ server/              ember server (Rust) — main server: agents, sessions, push
├─ proxy/               the IDE window wrapping layer (Python, FastAPI) — working
├─ extensions/          editor extensions: vscode-darkpyonix, vscode-darkpyonix-theme, intellij-darkpyonix
└─ LICENSE
```

---

## 📄 License

Apache License 2.0.
