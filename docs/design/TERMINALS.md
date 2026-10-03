# Persistent terminals (SPEC §P)

Status: implemented, **not yet compiled or run** (written without a build). Requirements:
`docs/SPEC.md` §P, FR-P1–FR-P6, NFR-P1.

Terminals, tasks and other processes started from an IDE window must keep running when every
window is closed, and must reappear — same scrollback, same screen, same running TUI — on any
device. To get that, **ember node owns the process**. The IDE windows (VS Code Web and the Ember
editor), agents and people are clients that attach and detach.

```
 VS Code Web tab ─ Pseudoterminal ─┐  wss /__terms/{id}/attach      ┌────────── ember node ──────────┐
 (companion web extension)         ├─ DarkPyonix proxy (same origin) ┤ session: PTY master, VT model,  │
 VS Code task ─ ember-term ────────┼──── ws://127.0.0.1 /v1/terms ───┤ clients, input order, control  │
 Ember editor ─────────────────────┤  (via ember server / P2P)        │      │ dup of the master fd     │
 agent tool call ──────────────────┘                                  └──────┼──────────────────────────┘
                                                                        PTY keeper (one per session,
                                                                        own process session; FR-P6)
```

## 1. ember node: sessions

Code: `node/src/term/` (`mod.rs` registry, `session.rs` one session, `screen.rs` VT model,
`pty.rs` PTY + keeper, `store.rs` metadata), routes in `node/src/api.rs`, wire types in
`node/src/proto.rs`, typed client in `node/src/client.rs`.

A persistent session is separate from `/v1/exec` (whose PTY lives as long as its WebSocket) and
from jobs (pipes, no terminal). It is created by one request and runs until the program exits or
someone kills it. Attaching and detaching never start or stop it.

| Concern | Behaviour |
|---|---|
| Create | argv (or the user's login shell: `$SHELL`, `-l` on macOS as VS Code does), cwd (inside the allowed roots), env (added to the daemon's, or `env_clear` + the client's whole env), initial size, **origin** (`ide-vscode`, `ide-ember`, `agent`, `user`), **project**, title, tags. `key` makes create idempotent: a running session with the same key is returned (`created: false`). `EMBER_TERM_ID` is set in the session; `EMBER_NODE_TOKEN` is removed. |
| Attach | Any number of WebSocket clients. Each gets `attached`, then a **snapshot**, then the live byte stream, which continues exactly where the snapshot ends (both happen under the session lock). |
| VT model | `alacritty_terminal` emulator fed with every byte. Scrollback: lines that scroll off are harvested and stored SGR-encoded (≈ text size), **10,000 logical lines** (soft-wrapped rows joined), capped at 3 MB. The snapshot is `ESC c` + scrollback + screen (primary, and the alternate screen when a TUI runs) + cursor, pen, input modes (app cursor/keypad, mouse modes, bracketed paste, focus, kitty keyboard flags, cursor style) + title. Gaps: scroll region, origin mode, saved cursor, charsets, tab stops, OSC 8 links — a TUI repaints these on its next full redraw (any resize causes one). |
| Input | All interactive clients may type. Input is queued to one writer thread under the session lock, so keystrokes from all clients reach the PTY **in arrival order**, and a program that stops reading never blocks the node. Terminal *answers* (cursor position reports, device attributes, focus events, OSC/DCS replies) are forwarded only from the client whose size the PTY follows, so the program gets exactly one answer; with no interactive client attached, the model answers. |
| Control | `take_control` (any interactive client, also from another controller) makes everyone else read-only; their input is **refused** with a `refused` event naming the controller. Released by `release_control` or when the controller detaches. Also over HTTP (`POST /v1/terms/{id}/control`) for UIs that hold the client id. |
| Size | The PTY (and the model) follow the **controller**; with none, the **most recently active** client (last to type, or an attach with `active: true`); with none, the last attached client that reported a size. A passive attach (`active: false`, e.g. restoring a terminal list) never resizes. Clients whose viewport differs render the PTY's size (`resized` events) and scale it to fit. |
| Slow clients | Each client has a 512-event queue; a client that falls further behind is dropped with `error` ("fell behind") and re-attaches for a fresh snapshot. A slow client never stalls the program or other clients. |
| Kill | SIGHUP to the terminal's foreground process group and the shell's group (what closing a terminal does), SIGKILL after 3 s; or an explicit signal. |
| Finished | `exit` is the last event to every client. The record stays listed (100 most recent finished); the 20 most recent keep their screen, so attaching to a finished task shows its output then `exit` (FR-P5). |
| Metadata | `<state>/terms/<id>.json` (atomic writes, dir `0700`): id, key, origin, project, argv, cwd, tags, pid, state, exit status, size, times, keeper socket. State dir: `EMBER_NODE_STATE_DIR`, default `~/.ember/node`. |
| Events | `term_started` / `term_finished` (with the full `TermInfo`) on `/v1/events`, next to the job events, so ember server keeps a per-computer list without polling. |

### Surviving restarts (FR-P6) — best effort

- **ember server restart, client restart, window close, VS Code server restart:** nothing happens
  to the session; ember server and the IDEs are only clients.
- **ember node restart:** the process survives **only through its PTY keeper.** When a session
  starts, node launches `ember-node __keep-pty --socket <state>/terms/<id12>.sock --pid <pid> --fd
  <n>` in its **own process session** (`setsid`), holding a duplicate of the PTY master. When
  node exits, the kernel does not hang up the terminal (an open master remains), so the shell
  keeps running. A restarted node reads the records, asks each live keeper for the master over
  the unix socket (`SCM_RIGHTS`) and **re-adopts** the session (`adopted: true`). Sessions whose
  process or keeper is gone are listed as `lost` (attach refused, kill/remove allowed).
  - Graceful stop (SIGTERM/SIGINT) writes each running session's snapshot to `<id>.snap`; the
    re-adopted model starts from it, so scrollback and screen carry over. After a **crash** the
    re-adopted session starts with an empty model (the next output or a resize-triggered TUI
    redraw fills it).
  - While no node runs nobody reads the PTY: the program runs until the kernel's PTY buffer is
    full, then blocks on output until node is back. Output in that buffer is not lost.
  - A re-adopted process is no longer node's child: its exit is noticed by polling and its exit
    status is unknown (`exit_code` and `signal` both null).
  - **Service managers must not kill the keepers with the daemon.** systemd: `KillMode=process`
    in the ember-node unit (the default `control-group` kills every process in the cgroup,
    including keepers and shells). launchd: `AbandonProcessGroup=true` in the plist, so stopping
    the job does not kill what remains of its process group (keepers and shells are in their
    own sessions already; this guards against launchd versions that track descendants). Neither
    unit file exists in this repository yet.
  - Disable with `EMBER_NODE_KEEP_PTY=0` (sessions then end with the daemon). Without a state dir
    there are no keepers and no records.
  - Why a minimal keeper rather than a tmux/dtach-style holder that also owns the model: the
    keeper is ~100 lines with a two-command protocol (`F`: send the fd, `Q`: quit), costs a few
    hundred kilobytes, and keeps one session engine in node. The cost is the crash case above.

### Memory (NFR-P1)

Per idle session in node: the emulator's visible grid (24 B/cell: 80×24 ≈ 46 KB; 200×60 ≈ 290 KB,
two grids while a TUI uses the alternate screen) + encoded scrollback (10,000 lines of typical
output ≈ 0.5–1.5 MB, hard cap 3 MB for pathological long coloured lines) + three parked threads
(PTY reader, writer, exit waiter; small stacks, mostly virtual) + alacritty's row cache, released
by `Screen::compact` after 10 s idle. Typical sessions stay under 2 MB; a session that printed
10,000 very long coloured lines can exceed it (bounded by the 3 MB cap). Not measured yet.

## 2. Attach API (for the Ember editor and any other client)

All routes need `Authorization: Bearer <node token>`. Over the DarkPyonix proxy the same routes
are under `/__terms` with the session cookie instead (section 3). JSON on the wire; binary data
is base64.

| Method | Path | Body → Response |
|---|---|---|
| POST | `/v1/terms` | `TermCreateRequest` → `TermCreateResponse` (201 created; 200 when `key` matched a running session) |
| GET | `/v1/terms?project=&origin=&running=` | → `[TermInfo]`, oldest first |
| GET | `/v1/terms/{id}` | → `TermInfo` |
| GET | `/v1/terms/{id}/snapshot` | → `{size, data (b64), text}` without attaching (409 when there is no screen) |
| GET (WS) | `/v1/terms/{id}/attach` | send `TermHello`, then `TermInput`s; receive `TermEvent`s |
| POST | `/v1/terms/{id}/control` | `{client, take}` → 204 (409 if refused) |
| POST | `/v1/terms/{id}/kill` | `{signal?}` → 204 (default SIGHUP, then SIGKILL) |
| DELETE | `/v1/terms/{id}` | → 204 (409 while running) |

```jsonc
// POST /v1/terms
{ "program": {"argv": ["/bin/zsh", "-l"]},       // or {"shell": "make -j8"}; omit: login shell
  "cwd": "/Users/me/proj", "env": {"EMBER_PROJECT": "/Users/me/proj"}, "env_clear": false,
  "size": {"rows": 40, "cols": 120},
  "origin": "ide-ember", "project": "/Users/me/proj", "title": "server",
  "key": "ember:run:dev-server",                   // optional: attach-or-create
  "tags": {"ember.panel": "3"} }

// TermInfo (list / get / attached.term)
{ "id": "3f2a…", "key": null, "title": "zsh", "origin": "ide-vscode", "project": "/Users/me/proj",
  "argv": ["/bin/zsh","-l"], "cwd": "/Users/me/proj", "pid": 4242, "state": "running",  // running | exited | lost
  "exit_code": null, "signal": null, "size": {"rows": 40, "cols": 120},
  "created_ms": 0, "finished_ms": null, "last_activity_ms": 0, "tags": {},
  "clients": [{"client": 7, "device": "VS Code on Mac (a1b2c3)", "kind": "vscode-companion",
               "pid": null, "read_only": false, "size": {"rows": 40, "cols": 120}, "attached_ms": 0}],
  "controller": null, "survives_node_restart": true, "adopted": false, "has_screen": true }
```

**Attach protocol** (one WebSocket per attached view):

1. Client → `TermHello`:
   `{"device": "Ember on iPad", "kind": "ember-editor", "pid": null, "size": {"rows":50,"cols":90},
   "active": true, "read_only": false, "snapshot": true}`.
   `active: true` when the user opened or focused the terminal (the PTY may resize to this
   client); `false` for background restores. `read_only: true` for viewers (agents watching).
2. Node → `{"type":"attached","client":7,"term":TermInfo}` (or `{"type":"error",…}` and close:
   unknown or lost session, bad hello).
3. Node → `{"type":"snapshot","size":{…},"data":"<b64>"}` (when asked). Write `data` to a fresh
   terminal of `size`; it starts with `ESC c`.
4. Then, in any order:
   `output {data}` · `resized {size}` (render at this size; scale to fit) ·
   `control {controller: TermClient|null}` (show "controlled by <device>"; disable typing when it
   is not you) · `refused {reason: controlled|read_only|not_running, controller}` (your input was
   dropped) · `clients {clients}` · `title {title}` ·
   `exit {code, signal}` (last; both null = unknown) · `error {message}` (last; "fell behind" →
   re-attach).
5. Client → `{"type":"input","data":"<b64>"}` · `{"type":"resize","size":{…}}` ·
   `{"type":"take_control"}` · `{"type":"release_control"}` · `{"type":"kill","signal":null}` ·
   `{"type":"detach"}` (or just close the socket). Closing never ends the session.

Guarantees: the snapshot and the stream neither overlap nor leave a gap; input from all clients
is applied in the order node receives it; every interactive client's terminal answers queries,
but only the size owner's answers reach the program. Rust clients use
`NodeClient::{term_create, terms, term, term_snapshot, term_attach, term_control, term_kill,
term_remove}`; `TermAttachment` splits into `TermSender` / `TermReceiver`.

**Ember editor checklist.** Terminal panel: `GET /v1/terms?project=<root>` (all origins, so
VS Code's and agents' sessions show with their origin badge) → attach each visible one with
`active: false`; on focus or first keystroke the node makes it the active client. "New terminal":
`POST` with `origin: ide-ember`, a fresh `key`, then attach `active: true`. Run actions: `program`
= the command, `origin: ide-ember`, `tags["ember.run"]`; show the exit status from `exit`. Closing a
terminal tab by the user = `kill`; the window going away = just close the sockets. Banner from
`control`; scale the PTY size to the viewport. Through ember server the same messages are relayed
over the node link.

## 3. VS Code Web (no VS Code source patch)

Two pieces, both on the VS Code extension/settings surface:

**a) Companion web extension — `proxy/companion/`** (integrated terminals). A `browser`-only
extension, so it runs in VS Code Web's web worker extension host. It contributes the terminal
profile **"Ember (persistent)"** (`contributes.terminal.profiles` +
`window.registerTerminalProfileProvider`) whose `TerminalProfile` carries a
**`Pseudoterminal`**: `open` creates a session (`origin: ide-vscode`, project = first workspace
folder path, a fresh `key`) and attaches over `wss://<proxy>/__terms/{id}/attach`; `handleInput`,
`setDimensions` and `close` map to `input`, `resize`, `detach`; `onDidChangeName` shows
"— controlled by <device>". On activation (`onStartupFinished`), on window focus and every 5 s
while focused, it lists the project's sessions and shows the ones not yet in the window with
`window.createTerminal({ name, pty, isTransient: true })` (passive attach): running VS Code
terminals from any device, plus VS Code tasks that finished in the last 10 minutes (their output).
It skips sessions whose `ember-term` runs in one of the window's own terminals
(`Terminal.processId` matches the session's `ember-term.pid`). `window.onDidCloseTerminal`
with `TerminalExitReason.User` kills the session (setting `ember.terminals.killOnClose`); a window
close or reload only detaches. Commands: *Ember: Attach to a Persistent Terminal…* (all sessions on
the computer, any origin), *New Persistent Terminal*, *Take/Release Control*, *Kill*, *Use Persistent
Terminals for New Terminals and Tasks* (writes the settings below). Unit tests:
`cd proxy/companion && node --test`.

The proxy side is `proxy/dpx/terms/` (`/__terms/*`, see its docstring): same-origin with the
workbench, so the session cookie authenticates both `fetch` and the WebSocket (which also checks
`Origin`); the node token comes from `~/.ember/node/local.json` and never reaches the browser; a
browser may create only `ide-vscode` / `ide-ember` sessions. Unit tests:
`cd proxy && python3 -m unittest tests.test_terms_relay`.

Install: copy `proxy/companion/` into the VS Code server's extensions directory as
`darkpyonix.ember-terminals-0.1.0/` (for `code serve-web`, the `extensions` folder under its server
data dir, or pass `--extensions-dir`), or package it with `vsce package` and install the `.vsix`.

**b) `ember-term` binary — `node/src/bin/ember-term.rs`** (tasks; also usable as a plain profile).
A small client that attaches the terminal it runs in to a node session: `ember-term` (new shell),
`ember-term -c "<cmd>"` (new session running `$SHELL -c`, exits with its status), `ember-term
attach <id> [--passive]`, `attach-or-create --key k [-- argv]`, `list`, `kill`. It forwards its
whole environment (so `options.env` of a task and `terminal.integrated.env.*` apply), tags the
session with `ember-term.pid` / `ember-term.mode`, and **detaches on SIGHUP** (VS Code disposing
the terminal), so the task keeps running. It finds the node through `EMBER_NODE_URL` +
`EMBER_NODE_TOKEN` or `~/.ember/node/local.json`.

Settings (user scope; the setup command writes the first two):

```jsonc
"terminal.integrated.defaultProfile.osx":   "Ember (persistent)",   // and .linux
// Tasks: VS Code runs automation shells as `<path> -c "<command line>"`.
"terminal.integrated.automationProfile.osx": { "path": "/Users/me/.cargo/bin/ember-term" }, // and .linux
// Lets ember-term tag the task with the workspace instead of its cwd.
"terminal.integrated.env.osx": { "EMBER_PROJECT": "${workspaceFolder}" }
```

Tasks (`tasks.json`, `type: shell` or `process`) then run in node sessions with `origin:
ide-vscode` and `ember-term.mode: task`. Closing the window: VS Code (eventually) disposes the task
terminal → ember-term gets SIGHUP and detaches → the build keeps running. Reopening on any device:
the companion shows the task's session with its scrollback; it ends with its exit status. VS Code's
own task state (problem matchers, "task is running") does not carry over to a re-shown session.

Debugging (FR-P5): a debuggee started with `"console": "integratedTerminal"` is launched through
the debug adapter's `runInTerminal` request, which VS Code serves with an integrated terminal of
the default shell, not with an extension profile — so the debuggee is **not** yet in a node
session. Making it one needs either `ember-term` as the default shell for `runInTerminal` or a
`DebugAdapterTracker` in the companion that rewrites `runInTerminal` (open problem below).

## 4. Tests (written, not run)

- `node/tests/terms.rs`: create / attach / detach / re-attach with snapshot; a session outliving
  every client (a socket dropped without detach); two clients typing alternately (both see both
  inputs in order); take control / refusal / release over HTTP / release on detach; size follows
  the controller, else the most recently active client (checked with `stty size`), passive
  attaches don't resize; kill (SIGHUP first; `exit` is the last event); a finished task replays
  its output then its exit status; remove; idempotent `key`; list filters; cwd outside the roots;
  re-adoption by a second node through the keeper with the snapshot of a graceful stop, and a
  `lost` record.
- Unit tests in `node/src/term/screen.rs` (round trips of plain text, colours, wide characters,
  alternate screen + modes, title, 10,000-line scrollback size, soft-wrap joining, shrink),
  `session.rs` (terminal-answer detection), `pty.rs` (fd passing), `config.rs` (endpoint file).
- `proxy/tests/test_terms_relay.py`, `proxy/companion/test/extension.test.js`.

## 5. Open problems

- Not compiled: alacritty_terminal 0.26 API details (see the report of the change), `portable-pty`
  0.9 `CommandBuilder::env_remove`, axum/tokio-tungstenite usage follow the existing code.
- Debuggee persistence (FR-P5) is not wired (above). Re-attaching the debugger is out of scope.
- The companion relies on the web worker extension host being same-origin with the proxy (true for
  this proxy, `proxy/docs/CONSTRAINTS.md` §3); a `vscode-cdn.net`-hosted worker would need a
  token handshake instead of the cookie. If needed, set `ember.terminals.proxyUrl`.
- VS Code cannot render an extension terminal at a fixed size: a passive viewer whose viewport is
  smaller than the PTY sees wrapped lines instead of a scaled view (the Ember editor can scale).
- Crash restarts of ember node lose the model (scrollback) of re-adopted sessions; exit status of
  re-adopted sessions is unknown. Windows has no PTY keeper (node's PTY code is unix-only).
- The memory figure of NFR-P1 and the ≤ 500 ms re-attach are estimated, not measured.
