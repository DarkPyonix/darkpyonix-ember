English | [한국어](https://github.com/DarkPyonix/darkpyonix-ember/blob/develop/docs/locale/README_ko.md)

# darkpyonix-ember

**One AI, many computers.** Ember is a development environment and remote IDE for coding agents.
Claude Code, Codex, Antigravity and OMP run on one main server that keeps every conversation.
Their tools act on whichever of your computers the work needs. A conversation can move to another
computer halfway through.

Ember is the client and the agent environment. DarkPyonix, underneath, is the Python kernel and
notebook runtime ([darkpyonix-core](https://github.com/DarkPyonix/darkpyonix)).

**User guide** (English and Korean): <https://darkpyonix.dev/darkpyonix-ember/>

---

## 🎯 Why

Three problems come up every day in agentic development.

- **A conversation is stuck on the computer that started it.** A CLI agent writes its transcript
  to local disk. Moving the work to another computer means leaving the conversation behind. Ember
  keeps every conversation on one main server, such as a Mac mini or a Raspberry Pi, so a
  conversation no longer belongs to one computer.
- **Agents from different vendors cannot talk.** Claude and Codex coordinate only through a shared
  file. Two Claude sessions on different computers or accounts cannot talk at all. Ember gives
  sessions a direct channel, across vendors, computers and accounts.
- **A remote computer's view of the network is hard to borrow.** Ember opens a browser whose
  traffic leaves from the chosen computer, with that computer's IP. Cookies and logins stay on the
  main server, so you keep one browser identity across many vantage points.

---

## 🧭 How it works

1. The main screen lists your **projects**. Each project shows its **sessions** with a status:
   running, waiting for approval, finished or failed. Your **computers** are listed below, with
   whether each is online.
2. You open a conversation to steer an agent. The agent runs on the main server, and its tools act
   on the session's current computer. You send messages, answer approvals, interrupt a turn, or
   move the session to another computer.
3. To read code, **Open IDE** opens VS Code or JetBrains Gateway on that project and computer.
4. You close the client, and the sessions keep running on the main server. A client opened later,
   on the same computer or another one, finds the conversation where you left it.

The client follows JetBrains Gateway: a light front door, with heavier windows opened on demand.
The reason is that agentic work is mostly watching and steering agents, not typing into an editor.

---

## 🏗 Architecture

Ember is three programs.

| Program | Runs on | Holds |
| ------- | ------- | ----- |
| `ember-server` | The main server | Every agent process, transcript, account, A2A queue, schedule and browser profile |
| `ember-node` | Every other computer | Project files, and the processes that tools start: builds, tests, servers, terminals |
| `ember-app` | Wherever you sit | A cache of what the main server last sent, so it starts instantly |

The client talks to `ember-server` over HTTP. `ember-server` reaches each computer over HTTP or
over a peer-to-peer transport built on [iroh](https://github.com/n0-computer/iroh) (QUIC, with hole
punching and a relay). The darkpyonix.dev hub lets a computer join your GitHub account with a
one-time code, so you do not type addresses or open ports.

---

## 🚦 Status

Ember has no releases yet. The table matches the code on `develop` as of 2026-10-03, as the
[guide's overview](https://darkpyonix.dev/darkpyonix-ember/en/index.html) records it.

| Area | Status | Notes |
| ---- | ------ | ----- |
| Sessions, transcripts, push, search, export | implemented | Forking a session is not built for any agent yet. |
| Claude Code and Codex | implemented | Driven through each CLI's own headless protocol. |
| Antigravity | partial | Gated by Ember's approval hook on agy 1.2.16. Shell writes fail under `--sandbox` ([#65](https://github.com/DarkPyonix/darkpyonix-ember/issues/65)). |
| OMP and other ACP agents | partial | Tested against a fake ACP agent; a real OMP run is pending ([#53](https://github.com/DarkPyonix/darkpyonix-ember/issues/53)). |
| Computers and switching | partial | Verified with a second node on the same Mac, not yet between two separate computers ([#6](https://github.com/DarkPyonix/darkpyonix-ember/issues/6)). |
| Project mount | partial | Written; not yet run on a real Mac mini or Raspberry Pi ([#6](https://github.com/DarkPyonix/darkpyonix-ember/issues/6)). |
| Peer-to-peer transport | partial | Tested on an in-memory network; the real-network measurement is pending ([#10](https://github.com/DarkPyonix/darkpyonix-ember/issues/10)). |
| darkpyonix.dev hub | partial | Tested against a fake hub. Its address is still being decided ([#62](https://github.com/DarkPyonix/darkpyonix-ember/issues/62)). |
| Native client | partial | Runs against `ember-server` over HTTP. Layout is unchecked on a real display, and phone clients are not built ([#9](https://github.com/DarkPyonix/darkpyonix-ember/issues/9)). |
| Open IDE | partial | VS Code and JetBrains Gateway open from a conversation. Ember's own editor is planned ([#32](https://github.com/DarkPyonix/darkpyonix-ember/issues/32)). |
| Persistent terminals | partial | [#26](https://github.com/DarkPyonix/darkpyonix-ember/issues/26) |
| Remote browser | partial | [#12](https://github.com/DarkPyonix/darkpyonix-ember/issues/12) |
| Releases and installers | planned | Every program is built with cargo for now ([#72](https://github.com/DarkPyonix/darkpyonix-ember/issues/72)). |

---

## 🚀 Build and run

Each crate under `crates/` is its own cargo project with its own `Cargo.lock`, so you build inside
the crate's folder. Work lands on `develop`.

```bash
git clone https://github.com/DarkPyonix/darkpyonix-ember
cd darkpyonix-ember
git checkout develop

# main server
cd crates/server
cargo build --release --locked
./target/release/ember-server

# client, in another terminal
cd crates/app
EMBER_SERVER_URL=http://127.0.0.1:8740 cargo run --release --locked
```

The HTTP listener has no login, so keep it on loopback (the default) or a network you trust.
[Install](https://darkpyonix.dev/darkpyonix-ember/en/install.html) covers `ember-node`, the
settings and what each program stores.

---

## ⚖️ Non-negotiables

| ID | Rule | Why |
| -- | ---- | --- |
| **E1** | The launcher and conversation screens never contain a webview. | The client is built natively on `dioxus-compose`; the IDE window is the one place a webview may exist. |
| **E2** | A wrapped agent keeps its native behaviour. Ember adds around it and never patches it. | Your agent settings, models and session IDs keep working as they do natively. |
| **E3** | Conversations, agent processes and account credentials live on the main server. | A computer is where tools run, so a conversation is never tied to one computer. |
| **E4** | VS Code is wrapped, never modified, and its Extension Host is never reimplemented. | Extensions then behave exactly as they do in VS Code. |
| **E5** | Inside the IDE window, a webview is scoped to the smallest region that needs it. | This applies once Ember's own editor lands; until then the IDE window as a whole is the exception. |
| **E6** | A Compose-native editor core never diverges from VS Code's behaviour. | Where the two disagree, VS Code is correct by definition. |

---

## 📦 Components

| Component | What it is |
| --------- | ---------- |
| **ember** | This repository: `ember-server`, `ember-node`, the client, and the IDE window. The IDE window wraps VS Code Web through `web/proxy/`. |
| **vscode-darkpyonix** | VS Code extension that renders DarkPyonix notebooks. Tested against a fake manager, not yet the real one. Installed by default in Ember's VS Code runtime. |
| **vscode-darkpyonix-theme** | VS Code theme in the DarkPyonix (phoenix) design language. In progress. |
| **intellij-darkpyonix** | IntelliJ and PyCharm plugin with the same notebook features. In progress, built and tested in CI. |

---

## 🔗 Related repositories

- **[dioxus-compose](https://github.com/DarkPyonix/dioxus-compose)**: the native GUI stack the
  client is built on. Its rules apply unchanged to Ember's client.
- **[darkpyonix-core](https://github.com/DarkPyonix/darkpyonix)**: the DarkPyonix kernel, manager
  and hub, with their API contracts. Ember links to these rather than redefining them.

---

## 🗂 Project layout

```
darkpyonix-ember/
├─ build/
│  └─ ose/               OSE: DarkPyonix's build of Code-OSS, the default IDE-window runtime
├─ crates/               one cargo project per crate
│  ├─ server/            ember-server: wraps agent CLIs, stores sessions, pushes updates
│  ├─ node/              ember-node: carries out tool actions on each computer
│  ├─ app/               ember-app: launcher and conversation UI on dioxus-compose
│  ├─ client/            client core: connection, sync and state below the UI
│  ├─ transport/         peer-to-peer connections (iroh backend and an in-memory fake)
│  ├─ hub/               darkpyonix.dev hub client: device registration and directory
│  ├─ bridge/            IDE window bridge: versioned webview and native messages
│  ├─ editor-conn/       Rust client for a Code-OSS server
│  └─ editor/            editor core session layer (planned editor, #32)
├─ docs/
│  └─ guide/             the user guide, served at darkpyonix.dev/darkpyonix-ember
├─ extensions/           vscode-darkpyonix, vscode-darkpyonix-theme, intellij-darkpyonix
├─ scripts/              repository checks and setup helpers
├─ tests/
│  └─ vectors/           recorded transcript and bridge test vectors
├─ web/
│  └─ proxy/             the VS Code Web wrapping layer (Python, FastAPI)
└─ LICENSE
```

---

## 📄 License

[Apache License 2.0](https://github.com/DarkPyonix/darkpyonix-ember/blob/develop/LICENSE).
