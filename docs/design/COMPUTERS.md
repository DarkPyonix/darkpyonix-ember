# COMPUTERS.md: computers, switching and tool interception (#6)

> Status: **implemented** (#34, #47, #48), tests run in CI; verified end to end with a node on the
> same Mac (Codex and Claude Code); a switch between two physical computers and the project mount
> (features `mount-nfs`, `mount-fuse`, not built in CI) are not verified (#6). Covers SPEC `FR-X2`, `FR-X3` and
> `FR-S7` v0 on ember server. The mechanism choice is in `INTERCEPTION.md`; this note records
> what was built, what it relies on, and what is still open. **[V]** = verified locally (evidence
> named), **[U]** = unverified.

## What exists

| Piece | Where | What it does |
|-|-|-|
| Registry | `ember/server/src/computers/mod.rs` (`Registry`) | Tables `computers(id, name, url, token, created_at)` and `session_computer(session_id, computer_id, env_json, notice, switched_at)` in `ember.db`, created by store migration 3 (`computers/schema.rs`) and read through the main `Store` connection, like `accounts`. |
| Service | `computers::Computers` | Register / list / probe (`/health`, `/env`) / remove; a session's current computer; `switch`. Installs three hooks on `Sessions`: an instructions hook (environment block), a start-config hook (Codex remote executor / Claude Code shell shim), and a message hook (the FR-S7 notice). |
| HTTP API | `ember/server/src/computers/api.rs` | `GET/POST /api/computers`, `GET/DELETE /api/computers/{id}`, `GET/PUT /api/sessions/{id}/computer`. |
| Codex relay | `ember/server/src/computers/relay.rs` | Loopback WebSocket (`ws://127.0.0.1:<port>/<secret>`) that codex app-server connects to; re-frames to the node's raw `/exec-server` stream. |
| Node bridge | `ember/node/src/exec_server.rs` | `GET /exec-server` (bearer auth) runs `codex exec-server --listen stdio` per connection and relays bytes; `ember-node exec-server` runs the same command on its own stdio. |
| Browser egress | `ember/server/src/computers/egress.rs`, `ember/node/src/egress.rs` | Per computer, a loopback SOCKS5 listener (`socks5://127.0.0.1:<port>`) that a project's browser uses as `--proxy-server`; each connection becomes one node `/egress` stream where the node runs SOCKS5 (FR-R1, see `REMOTE-BROWSER.md`). A computer that is some project's browser egress cannot be removed (409). |
| Claude shim | `ember/server/src/computers/shim.rs`, `ember/server/src/bin/ember-exec.rs` | `CLAUDE_CODE_SHELL_PREFIX` target; runs Bash-tool commands on the node via `/exec` and carries the cwd back. |
| Mount | `ember/server/src/computers/mount.rs` (+ `mount/`) | Mounts a Claude Code session's cwd from its node at the same path: `remote_fs.rs` (node-backed filesystem with caches), `nfs.rs` (loopback NFSv3, feature `mount-nfs`), `fuse.rs` (Linux FUSE, feature `mount-fuse`), `cmd.rs` (mount commands, mount point checks). **Written, not compiled or run**; see § Project mount. |

The local server is the implicit computer `local`. A session with no `session_computer` row
behaves exactly as before this change: no environment block, nothing redirected.

## Switching (FR-X3, FR-S7 v0)

`PUT /api/sessions/{id}/computer {computer_id}`:

1. Refused with 409 while the session is mid-turn (`running` or `waiting_for_approval`), 404 for
   an unknown computer or session, 502 when the target fails `/health` or `/env`.
2. Records the computer and its `/env` snapshot.
3. If the session's agent has already run (it has a native id), queues a notice: the computer
   changed and earlier file contents, listings and command output must be re-read / re-run.
4. Releases the agent process. The next message starts it again (native resume) with:
   - the **environment block** built from `/env` as system-level instructions: Claude Code
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

**Codex: tools run on the node (shell, unified exec, PTY, apply_patch file ops [U]).**
The start request carries `RemoteExec {environment_id: "ember-<computer id>", exec_server_url}`.
The adapter then sends `initialize` with `capabilities: {experimentalApi: true,
requestAttestation: false}`, `environment/add {environmentId, execServerUrl}`, `thread/start`
with `environments: [{environmentId, cwd}]`, and `environments` again on every `turn/start`
(`thread/resume` has no such field). The app-server process starts in the temp directory when
the project path does not exist on the server.

**Claude Code: Bash runs on the node; file tools go through the project mount.**
Environment: `CLAUDE_CODE_SHELL_PREFIX=<abs path of ember-exec>`, `EMBER_EXEC_NODE_URL`,
`EMBER_EXEC_NODE_TOKEN`, `EMBER_EXEC_REMOTE_SHELL` (the node's `$SHELL`), and with the mount
enabled `EMBER_MOUNT_CTL`. Read / Edit / Write / Glob / Grep operate on the server's disk, where
the session's cwd is mounted from the node (§ Project mount); with the mount off (`EMBER_MOUNT=off`
or a build without the features) the project path must exist on the server as before (the
adapter refuses to start otherwise). Hooks and stdio MCP launches, which
the prefix also wraps, run locally by default (`EMBER_EXEC_NON_TOOL=remote` sends them too).

## Project mount

Claude Code's file tools run inside `claude` on the server, so the node's project directory is
mounted on the server **at the same absolute path** (`INTERCEPTION.md` option (a)).

**When.** A `Sessions` prepare hook (new: async, awaited before the start-config hooks) runs
before every agent start. For a Claude Code session whose current computer is a node it mounts
the session's cwd, or joins an existing mount of that computer covering it; on `local` it
releases the session's mount. A switch releases the old mount after stopping the agent. A sweep
(every 30 s) unmounts a mount none of whose sessions has had a live agent for 2 minutes; shutdown
unmounts everything. Mount points this code created are removed again.

**Mechanism per server OS.**

| Server | Mechanism | Crate | Mount syscall privileges |
|-|-|-|-|
| macOS (Mac mini) | NFSv3 server on `127.0.0.1:<ephemeral>` inside ember server, mounted with `/sbin/mount_nfs` | `nfsserve` 0.11 (BSD-3; started at XetHub, now github.com/huggingface/nfsserve) | none on a mount point the user owns **[U]** (nfsserve's README and ext4nfs show `mount_nfs` without `sudo`); no kext, unlike macFUSE |
| Linux (Pi) | FUSE in-process | `fuser` 0.18 (MIT; pure-Rust, no libfuse, uses setuid `fusermount3`) | none on an owned mount point; needs package `fuse3` and readable `/dev/fuse` |
| Linux, fallback | the same NFS server + `mount -t nfs` | `nfsserve` | root (or an fstab `user` entry) |

Considered and not chosen: macFUSE (kext / user approval on every macOS update), FUSE-T (an
NFS shim around FUSE that would be an extra install, and its server is not open source **[U]**), `mount_webdav` / `mount_smbfs` (weaker POSIX
semantics: no symlinks / modes over WebDAV, SMB server work is larger), FSKit (macOS 15.4+ app
extension, not a daemon library).

**Mount points (one-time privileged setup).** The mount point must exist and be owned by the
server user. Missing directories are created when the nearest existing parent is writable (e.g.
node and server share the user and `/Users/<u>/…`). Otherwise the start fails with a hint, and
`ember/server/scripts/ember-mount-setup.sh <path>` (run once with `sudo`) prepares it:
- macOS `/Users/<other>/…`: `mkdir -p` + `chown`.
- macOS `/home/<u>/…`: `/home` is an autofs mount (`auto_home`); comment out its line in
  `/etc/auto_master` and `automount -vc`, then `mkdir` + `chown`.
- macOS new top-level directory (`/srv`, `/workspace`): a line in `/etc/synthetic.conf` and a
  reboot (the system volume is read-only), then `mkdir` + `chown`.
- Linux `/Users/<u>/…` (a Mac node's paths on the Pi) or `/home/<other>/…`: `mkdir -p` + `chown`.

A non-empty local directory at the same path is not mounted over (it would hide it, and a local
session might use it) unless `EMBER_MOUNT_SHADOW=1`. A stale mount from a crashed run is
force-unmounted first.

**Node API added for it** (all under the path policy and the write lock): `/fs/lstat`,
`readlink`, `symlink`, `mkdir` (`parents`, `mode`), `remove` (`recursive`; never a root; links,
not targets), `rename` (`overwrite`), `setattr` (chmod, truncate, utimes in one call), `pwrite`
(in-place positional write). `Stat` and `DirEntry` gained `ino`, `nlink`, `atime_ms`,
`ctime_ms` / `mode`; error bodies carry a portable `errno` name (`ENOTEMPTY`, …) because errno
numbers differ between macOS and Linux.

**Reaching the node.** The mount opens the node's file API the way `Computers` opens every node
client (`node_client`, via `Computers::node_fs`, which `main` passes to
`ProjectMounts::from_env`): over HTTP for a computer registered by URL, over the peer-to-peer
transport through the server's `Dialer` for one registered by peer address (FR-N1). Either way
the client carries the mount's 4 s per-request deadline (`NodeClient::with_deadline`; over the
transport it covers stream open, including a dial when no connection is cached, the HTTP
handshake, the request and the whole response body; over HTTP reqwest's own timeouts are set
too). A missed deadline is `ClientError::Timeout` → `ETIMEDOUT`; a failed dial or stream is
`ClientError::Transport` → `EIO`; both count as an outage. Tests: `ember/server/tests/peer_mount.rs`
(a real node and a stalled peer on `MemNetwork`).

**Caching and freshness.** Inode numbers are the mount's own (path ↔ id, stable across renames).
Attributes and listings live 1 s (`EMBER_MOUNT_TTL_MS`); a listing fills the attribute cache.
Files up to 4 MiB are fetched whole on first read and served from memory while size and mtime
match; larger files by range. Writes go through to the node at once (`pwrite`), so nothing is
lost if the server dies. The kernel side uses `actimeo=1`, `nonegnamecache` (macOS) or a 1 s FUSE
TTL. After every Bash command `ember-exec` sends `invalidate` to a private datagram socket
(`EMBER_MOUNT_CTL`), so what Bash changed on the node is read fresh; other changes on the node
(the user's editor) show within about 1 s.

**Failure behaviour.** Every node call has a 4 s deadline; NFS mounts are `soft,intr,timeo=50,
retrans=2,retrycnt=0,deadtimeout=60` (macOS), so a lost node surfaces as `EIO` / `ETIMEDOUT` from
the syscall within seconds (worst case ~15 s), never a hang in uninterruptible I/O; NFS never gets
`JUKEBOX` (which retries forever). The FUSE front end always replies. If ember server dies, the
NFS mount is force-unmounted by the kernel after `deadtimeout`; FUSE returns `ENOTCONN`. The
outage is also put in front of the agent's next message once as a system notice, and a start
whose node is unreachable fails with "computer … is unreachable".

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

- **Run end to end on one Mac only** (#6, 2026-10-03): a Codex turn and a Claude Code Bash call
  ran on a node registered as a separate computer. Still to verify on the Pi.
- **Peer-addressed mount: not run against a real mount.** `NodeClient::with_deadline` and
  `Computers::node_fs` build in CI; `ember/server/tests/peer_mount.rs` needs a mount feature,
  which CI does not enable. Also open: a timed-out
  request over the transport drops its hyper `SendRequest`, but the background connection task
  of a peer that never answers may keep its stream open until the transport connection ends
  **[U]**; and the dialer keeps its cached connection after a timeout, so a silently dead path
  costs one deadline per request until the transport's own idle timeout closes it and the next
  request re-dials (forgetting the connection on timeout would also cut healthy concurrent
  requests). WebSockets (exec, terminal attach) still have no deadline on either reach.
- **Project mount not built in CI or run** (features off by default). Written against `nfsserve` 0.11.0 and `fuser` 0.18.0
  read from their published sources. To verify on the Mac mini and the Pi: non-root
  `mount_nfs` on a user-owned directory, the exact `mount_nfs` option names (`deadtimeout`,
  `nonegnamecache`, `actimeo`, `retrycnt`), macOS NFS client behaviour with nfsserve
  (readdir without `.`/`..`, `ACCESS` always granted), and latency of Grep over the mount on the Pi.
- **Mount security**: the loopback NFS server accepts any local connection (nfsserve does not
  check the peer or AUTH_UNIX credentials), so another local user who finds the port could mount
  the node's project. Fine on a single-user Mac mini; otherwise it needs a peer-uid check in a
  fork of nfsserve's accept loop. FUSE mounts are owner-only by default.
- **Mount scope**: only the session's cwd is mounted. Paths outside it (a sibling repo,
  `~/.gitconfig` on the node) are still the server's. Nested project directories of two sessions
  on one computer are refused; sessions of two computers on one path conflict.
- **No change feed** on the node: external edits are seen after the TTL; a node fs watcher
  pushing invalidations over `/events` would make it exact.
- **Exclusive rename / hard links / xattrs / locks** are not supported through the mount
  (`RENAME_NOREPLACE` → `EINVAL`, locks local only).
- **Claude cwd tracking**: the shim writes the node's `pwd -P` to the local cwd file; with the
  mount the directory exists here only if it is under the mounted cwd.
- **Token exposure**: the node token sits in the Claude process environment (and so in its local
  hooks' environment). A per-session scoped token is the fix.
- **Codex needs a codex binary on the node** (outside FR-X5's "ember node only"), and the
  exec-server is not confined by the node's path policy.
- **Node protocol**: `/exec-server` and `/egress` were added to the node API (additive; the node reports no protocol number).
- Codex `additionalContext` (keyed context on `turn/start`) may be a cleaner carrier for the
  environment block than `developerInstructions`; not tried.
- Switch is refused mid-turn; whether a user should be able to force it is open (`INTENT.md` Q2).

## Adding a computer with an enrol code

`FR-C1`, `FR-C2` and `FR-C2a` to `FR-C2k`, `INTENT.md` D39 ("Confirm at join"), issues #491 and #526. The design was approved by the leader on 2026-11-06 [user, relayed by the supervisor]; the full memo is `.scratch/pz/enrol-door-design.md` (labels D1 to D13 below are the memo's). Today a computer is added by pasting a long-lived node token into `POST /api/computers`. The enrol code replaces the paste: the server mints a one-time code, the owner runs one command on the other machine, confirms the machine in the app, and the machine registers itself.

### What is built (FR-C1, #523)

Code: `ember/server/src/computers/enrol.rs`. Merged in #523; CI compiled it and ran the `fr_c1_*` tests. It has not run between two real machines.

Step 1 of FR-C2 (merged in #543, CI-tested; the follow-up on branch `fix/enrol-followups` is written, not compiled) changes three of the bullets below: `redeem(code, node_info, now, source_key)` now moves the row to `awaiting_confirmation` and answers `{enrol_id, poll_secret, expires_ms}` (the poll secret stored only as a SHA-256 hash); `status` gives `waiting`, `awaiting_confirmation`, `joined`, `denied` or `expired`, and `GET /api/computers/enrol-codes/{id}` adds the untrusted node information; `joined` now needs the owner's confirm and the node's delivery, not `complete` alone. The failure limiter gains the global cap of 60 failures per 10 minutes.

- Store migration `crate::computers::enrol::MIGRATION` (appended last to `store::MIGRATIONS`): `enrol_codes(id, code_hash BLOB UNIQUE, name, created_ms, expires_ms, used_ms, computer_id)`. The code is never stored, only its SHA-256 digest.
- `Enrol::mint(name, now, ttl, base_url)` returns `{id, code, expires_ms, command}`. The code is `dpe_` plus 244 random bits made like the channel invite secrets (`channels::invites::new_secret`). The lifetime defaults to 10 minutes and is clamped to 10 minutes; zero is refused. The name is checked like a computer name (not empty, not `local` or `this server`) plus the guest-name shape rules. Expired rows older than one hour are deleted on mint.
- `command` is `ember-node enrol <code> --server <door url>`, plus ` --server-fingerprint <full 64 hex server peer id>` when the transport is on (the pin; the short form is for human screens only). The URL is the enrol DOOR's, not the owner API address (that port serves no `/enrol/redeem`): `EMBER_ENROL_PUBLIC_URL` when set, else `http://<EMBER_ENROL_LISTEN>` when that names one interface. `main.rs` derives it from the enrol door (`EnrolDoor::door_url`); `EMBER_PUBLIC_URL` is not used for enrol. The 201 answer also carries `door: "on"`, `server` (the door URL) and, with the transport on, `server_peer_id` and `server_fingerprint` (both omitted, never null, when it is off). The warning says the code is shown once, names the door URL and, for `http`, that the door speaks plain HTTP. With the door off the mint answers 409 `door_off` and creates no code; with the door on and no usable URL it answers 409 `door_url_unknown` and creates no code. Step C3c, written, not compiled; [provisional, 2026-11-06, leader approval relayed by the supervisor; owner confirmation pending]. The URL is checked to be plain http(s) with no shell characters, because it lands in a command line.
- `Enrol::redeem(code, now, source_key)` runs one `UPDATE ... WHERE code_hash = ? AND used_ms IS NULL AND expires_ms > now RETURNING id, name`. Unknown, used, expired, cancelled and malformed codes all return the same `Refused::Generic` ("this enrol code cannot be used"). Two concurrent redeems cannot both win, because the statement is the check and the mark. The compare happens inside SQLite on the digest, so no secret is compared in Rust and no constant-time compare crate is needed.
- Rate limit: 10 failed redeems from one source key in 60 seconds, then `Refused::RateLimited` for that source, decided before the code is looked at, so it says nothing about the code. The guest door's `AddrLimiter` counts accepts by kind and cannot be reused for failures without a refactor, so `enrol.rs` has a small failure window (at most 4096 sources tracked). Success is not counted.
- `Enrol::status(id, now)` gives `waiting`, `joined` or `expired` by enrol id, never by code. `joined` needs the owner's confirm (`Enrol::confirm`, which records the computer; there is no public `complete`) and the node's delivery. `Enrol::cancel(id)` deletes an unredeemed code.
- Admin routes, merged into the local token-protected router only (not the transport `app`, not the guest door): `POST /api/computers/enrol-codes {name, ttl_secs?}` answers 201 `{id, code, expires_ms, command, warning}` with `Cache-Control: no-store`; `GET /api/computers/enrol-codes/{id}` answers `{status}`; `DELETE /api/computers/enrol-codes/{id}` answers 204, or 409 when the code was already redeemed.
- The code is in no log: `EnrolCode` and `Minted` print a redacted `Debug`, and the mint log line names the enrol id only.

### Decided design (FR-C2, not built)

**Door (D1, D2).** The redeem route cannot sit behind `auth::protect`, and a `token_exempt` path on the owner listener would put an unauthenticated write route next to every admin route. So the enrol door is a listener of its own, like the guest door: `EMBER_ENROL_LISTEN`, default OFF, `EMBER_ENROL_LISTEN_ALLOW_REMOTE`, `EMBER_ENROL_TRUSTED_PROXIES`, and `EMBER_ENROL_PUBLIC_URL` for the URL in the printed command. It serves exactly `POST /enrol/redeem` and `GET /enrol/status/{enrol_id}` and answers 404 for the rest. When the door is off, the mint answer says `door: off` and prints no command. A restart turns it on; auto-close is Later. Option (b), redeem over the `FR-N5` transport, is not built (D10): it would weaken the rule that every transport caller is authenticated, and a machine that already has a device key uses the hub device-link route (`POST /api/hub/devices/{id}/computer`).

**Confirm at join (D3, D4, D5).** The review of #523 asked that the owner see the computer and confirm before it gets what D37 grants. The redeem therefore releases no credential:

```
node                         enrol door                    server core / admin router        app (owner)
 |                               |                                 |                              |
 |                               |                                 |<-- mint (name) --------------|
 |                               |                                 |--- code, command, warning -->|   state: waiting
 |<============ owner copies the command to the other machine ====================================  |
 |-- redeem {code, node_info} -->|                                 |                              |
 |                               |-- redeem (atomic) ------------->|                              |   state: awaiting_confirmation
 |<-- {enrol_id, poll_secret} ---|                                 |                              |   (deadline = redeem + 10 min)
 |-- poll (Bearer poll_secret) ->|-- status ---------------------->|                              |
 |<-- awaiting_confirmation -----|                                 |--- GET status: node_info, -->|   host name, observed address,
 |                               |                                 |    observed_addr, fingerprint|   fingerprint, rename
 |                               |                                 |<-- confirm {name?} ----------|
 |                               |                                 | name-clash check (409), mint |
 |                               |                                 | token, register, complete    |
 |-- poll ---------------------->|-- status ---------------------->|                              |
 |<-- {token, name, peer id} ----|    (delivered ONCE, then wiped) |                              |   state: joined
 |  write enrol.json 0600        |                                 |                              |
 |-- ember-node starts, serves the node API with the token; the server connects and lists it online -|
```

States: `waiting`, `awaiting_confirmation`, `joined`, `denied`, `expired` (`FR-C2a`). A deny, or no answer within 10 minutes of the redeem, registers nothing. A redeem burns the code, so a denied, failed or expired enrolment needs a new mint.

- **Why a poll and a server-minted token (D3).** Releasing the credential at redeem would give it to a node before any owner act. Holding the redeem request open runs into the request timeout and lets one redeem pin an in-flight permit. A node-chosen token would make the node send a secret that controls itself to a server nobody has confirmed. The `poll_secret` is 256 random bits, stored as a SHA-256 hash, and can only ask this enrolment's status.
- **What the owner sees (`FR-C2c`).** The owner-chosen name, the host name the node reports, the address the server observed (after the trusted-proxy rule) and a peer-id fingerprint, all rendered as plain text. A mismatch warns and never blocks. The owner can rename at confirm. Confirm checks the name against the registry again and answers 409 on a clash; the enrolment stays `awaiting_confirmation` and the same call takes a new name.
- **Address.** The node's `url` or `peer_addr` after validation. If it sent neither, the server proposes the observed address with port 8741 and the app shows it for editing; it is never trusted automatically (D11, [provisional, pT/pX]; whether 8741 is fixed in every deployment is unknown). The server probes `GET /health` once at confirm and reports reachability without blocking.
- **Registration is an owner act.** The door never calls `Computers::register*`; the confirm route does, and rolls the registration back if recording it on the enrolment fails. The registry name check and `register` are separate steps, so another writer can take the name in between; `register` answering name-taken is the authoritative answer (409 `name_taken`, state unchanged). A `confirmed` enrolment that expires (the node never polled within 10 minutes, or the credential was lost to a restart) has its computer removed from the registry, once and idempotently, so the same name can be enrolled again; the settle does it on any read or sweep and the router sweeps at startup. A failed removal is logged at warn without a secret and does not stop the state change. A `joined` computer is never removed. The failure limiter keys a source by its first 64 characters on both lookup and record.
- **Limits (D6, `FR-C2f`).** A global failed-redeem cap of 60 per 10 minutes across all sources, so rotating source keys cannot reset the per-source window; door bucket 5 per second with burst 10, 8 in flight, 32 connections, 4 KiB body. The numbers live in a configurable struct. The cost is that a loud attacker can block a real enrolment for a while; the owner re-mints.
- **Logging (`FR-C2g`).** The code, poll secret and token appear in no log; lines name the enrol id and the source address. An id taken from a request is logged only after `log_id` (at most 64 characters, every character other than an ASCII letter, digit, `-` or `_` replaced by `_`), so attacker-chosen text cannot inject a line or an escape sequence.
- **Poll order (`FR-C2f`, `FR-C2g`).** The poll route verifies the secret first with the side-effect-free `Enrol::verify_poll`, limits only the polls that verified (1 per 2 s per enrolment), and only then calls `take_delivery`; a limiter is never consulted before verification, so invented ids cannot fill its 1024 slots, and a rate-limited poll never consumes the credential.

**Node side (D9, D13, `FR-C2h`).** `ember-node enrol <code> --server <url>` shows the server and asks "Continue? [y/N]" unless `--yes`. Plain http on a loopback or private address is allowed; a public address needs `--allow-http`. The node warns about plain http (`FR-C2i`), polls every 2 s, and on delivery writes `enrol.json` (0600) in its state dir and the server's peer id into `allowed-peers`. `EMBER_NODE_TOKEN` wins over `enrol.json`. N1 is merged (#609, CI compiled and ran its tests); its hardening (N1b) is merged (#613): `ember/node/src/enrol.rs` reads and validates `enrol.json` (a symlink, a non-regular file or a file of another user is refused) and writes it with `allowed-peers` under an `.enrol.lock` on the state dir so that both change or neither does (one atomic rename, previous file put back if the second rename fails, no backup file), and `config::token_with_source` implements the fallback and logs which source won. N2, the `ember-node enrol` command (`ember/node/src/enrol_cli.rs`, arm in `main.rs`), is merged (#614, CI compiled and ran its tests). The follow-up N2b is written, not yet compiled: a preflight after the lock and before the redeem refuses what the final write would refuse (an unreadable `enrol.json` when replacing, a bad `allowed-peers`, a state dir that is not writable), Ctrl-C while waiting exits with a message that the code was already redeemed (the server has no cancel route, issue #617) and is handled, not fatal, after the delivery, the help recommends `-` over an argument for the code (a code in argv shows in `ps` and in shell history), and a `NotSynced` write still prints the "accepts commands" lines. The review follow-ups below came after the #614 CI run and are not yet compiled. Follow-ups: `write_enrolment` returns a typed `WriteError` (`Refused`, `RolledBack`, `NewKept`, `NotSynced`) and a `WriteReport { peer_added }`, and after the server delivered the credential every outcome that is not a full success tells the owner what became of the token (lost and how to enrol again, or on disk); a failed directory fsync keeps both files; `allowed-peers` is read with a 64 KiB cap; the lock file must be a regular file owned by this user. Limits: `config::from_env` reads `enrol.json` without the lock on purpose (one rename, so a reader sees the old or the new complete file); `sync_all` on macOS is not `F_FULLFSYNC`; `flock` on NFS may be advisory-only or unsupported and the lock is per host. Flags: `<code>` (or `-` for stdin), `--server`, `--server-fingerprint` (full 64-hex id preferred; the 16-hex short form is accepted with the notice "short pin: 64 bits"), `--trust-unverified-server`, `--url`, `--transport`, `--state-dir`, `--yes`, `--allow-http`, `--replace`. R1 to R4: a matching pin trusts the delivered id; a mismatch, a pin with no delivered id, or a malformed id aborts with nothing written, prints both fingerprints and the name and id of the computer the server already registered, and says to remove it in Ember; with no pin an interactive run asks for the fingerprint, and a non-interactive run or `--yes` trusts only with `--trust-unverified-server` (`--yes` never grants trust); an untrusted id is not added to `allowed-peers` and the node says how to add it. The run locks the state dir before the first request, follows no redirects, uses no proxy, reads at most 64 KiB of a response, prints a plain-http warning for every http server and refuses plain http to a public address without `--allow-http` before any request. A refused code prints exactly `this enrol code cannot be used`; a non-interactive run without `--yes`, or over an existing `enrol.json` without `--replace`, is refused. Tests: the unit tests `fr_c2_node_cli_*` in `enrol_cli.rs` and `ember/server/tests/fr_c2_enrol_node.rs` (the real door on loopback). Not verified: HTTPS, two real machines, a real terminal. [provisional, 2026-11-06, leader approval relayed by the supervisor; owner confirmation pending] A revoked computer leaves `enrol.json` on the node; no command removes it (delete the file by hand), and a re-enrol needs a new code and `--replace`. The node's reqwest names the `rustls-tls` feature explicitly (N1), so https servers work with no cargo feature to set.

**Warning text.** The mint answer says: "The enrol door speaks plain HTTP. Anyone on the network path can read the code and the credential. Use it on a trusted network, or put TLS in front of it and set EMBER_ENROL_PUBLIC_URL to the https address. If the door is off, this command will not work." The node prints, for every http server including loopback and private ones, exactly the text of `PLAIN_HTTP_WARNING` in `ember/node/src/enrol_cli.rs`: "This server speaks plain HTTP. Anyone on the network path can read the enrol code, the poll secret and the credential this node receives, and whoever reads the poll secret can fetch the credential before this node does. Use a network you trust, or an https address." It adds the poll secret and the race for the credential, so the contents match but the words differ. [provisional, 2026-11-06, leader approval relayed by the supervisor; owner confirmation pending]

**First contact (TOFU).** The node cannot verify the server on first contact: nothing it holds yet links the delivered peer id to the machine at the URL. What pins it is the full id in the command the app prints (`--server-fingerprint <64 hex>`, R1). Without a pin there is no automatic trust: an interactive run asks y/N (the answer must be a deliberate y; n, Enter or Ctrl-D declines), and a non-interactive run or `--yes` trusts only with `--trust-unverified-server`; otherwise the id is not added and only the token-only HTTP path is set up. Residual risks that remain: a rogue server at the URL can still hand the node its own token and so take over the node's HTTP path (it receives the code and the node info as well), and `https` plus the pin are the mitigations. Known limits: the plain-http warning also prints for loopback and private-range doors, where it is mostly noise; `--trust-unverified-server` still asks when the run is interactive. [provisional, 2026-11-06, leader approval relayed by the supervisor; owner confirmation pending] 

### Threat summary and residual risks (`FR-C2j`)

| Risk | What stops it | Residual |
|-|-|-|
| Code leaked (screenshot, chat, LAN sniffer) | Single use, 10 minutes, owner confirm with node details, deny | A sniffer that wins the redeem gets the `poll_secret`; the owner must read the details |
| Malicious or confused node with a valid code | Owner confirm, owner-chosen name, deny, removable at once | The owner misjudges; the card puts details in front of the owner |
| Plain http | TLS in front is the owner's duty; warnings on both sides; `--allow-http` for public addresses | Code, poll secret and token cross the LAN in clear; sealed delivery is Later (D7) |
| Rogue server URL | Command copied from the app; node shows the URL and asks; TLS | The node cannot verify the server; a rogue controller can run commands within the node's roots. Fingerprint pinning is Later (D8) |
| Door open by mistake | Default OFF, startup error without ALLOW_REMOTE, global failure cap, no admin route on the door | Cleartext, and a bounded DoS; auto-close is Later (D12) |
| Replay, oracle | One atomic statement, one generic refusal, poll 404 for unknown id and wrong secret, constant-time compare, token wiped on delivery | None found |

### Build order

1. Confirm at join in the core and the admin confirm and deny routes, the global failure cap, name-clash check at confirm. No door needed. **Merged** (#543, #548): migration `enrol::CONFIRM_MIGRATION` (columns on `enrol_codes`, no token column), `Enrol::redeem` returning `{enrol_id, poll_secret, expires_ms}`, `confirm`, `deny`, `take_delivery`, `status_detail`, `Registrar` trait, `LimitConfig` with the global cap, routes `POST .../{id}/confirm` and `.../deny`. The pending token lives in memory only; a restart before delivery makes the enrolment `expired`. The name-clash check at mint is not done yet (the check at confirm is the arbiter).
2. Door skeleton and limits (shared module extracted from `guest_door.rs`; guest tests stay green). Security review. The shared module is merged (#562, #566, #569, #571). The skeleton `EnrolDoor` (`src/enrol_door.rs`, wired in `main.rs` as its own task) is merged: the listener, the guards and the limits. Step C3a (below) adds the routes.
3. Redeem and poll routes. Security review. **C3a is written, not compiled**: `POST /enrol/redeem` and `GET /enrol/status/{enrol_id}` in `src/enrol_door.rs` over the one shared `Arc<Enrol>`, the poll limit (1 per 2 s per enrolment, at most 1024 entries, fail closed) in the door, and a shape check of the code in `Enrol::redeem` (a code that no mint makes is refused without a SQLite lookup and spends the failure budgets like any failure). A request first passes the limit layer, the timeout, the Host and Origin guard and the body limit, which can answer 429, 503, 408, 403, 400, 405 and 413; what reaches the router is 404 except the two routes. **C3b** (`server_peer_id` in the delivery) and `ember-node enrol` (step 4) are not built.
4. `ember-node enrol`, the token-file fallback and the explicit `rustls-tls` feature of reqwest; dependants' `Cargo.lock` files in the same PR.
5. App: mint dialog with command and warning, the "wants to join" card, rename on a name clash.
6. A real two-machine run by the owner (LAN, then through a TLS proxy). It gates marking `FR-C1` and `FR-C2` verified.

### Later

- **D12, door auto-close.** Open and close the door at runtime for the life of a pending code (needs a bind and unbind path the guest door lacks).
- **D7, sealed delivery.** The node sends an ephemeral X25519 key in `node_public_info`; the server seals the credential with an AEAD to it. Removes the passive-sniffer case; an active MITM still needs TLS.
- **D8, server fingerprint pinning.** `--server-fingerprint` in the command, checked against the server's reply; the server now exposes its peer id in the `joined` answer (C3b, written, not compiled), so the pin can be checked once `ember-node enrol` exists; requirements for the node step, in full: R1 pin first: the command the mint prints carries `--server-fingerprint <full 64 hex peer id>`, and the node compares the delivered id with it BEFORE writing anything; a mismatch aborts, writes nothing and prints both fingerprints. R2 no pin means no automatic trust: the node prints the delivered id's fingerprint and asks whether it matches the server's console or app; non-interactive use needs `--server-fingerprint` or an explicit `--trust-unverified-server` (its help text and a warning say that it widens the node's reach to an unverified server), and by default the id is NOT added to `allowed-peers` (the token-only HTTP path still works as before). R3 verify, then write: `enrol.json` (0600, tmp and rename) and `allowed-peers` together or neither; the id is shape-checked (64 lowercase hex) first and only that one id is added. R4 the node prints what it will allow: commands from this server's fingerprint over the transport and from the token holder over HTTP. The `ember-node enrol` parser that enforces R1 to R4 is NOT BUILT (the node step, a later PR with its own tests); until it exists no node consumes the id, so the delivery adds no reach by itself.

### Real-hardware run (FR-C2k)

A checklist for the owner's two-machine run. Machine S runs `ember-server`, machine N is the computer to add; both are on one LAN, N is reachable from S on port 8741. Everything below is built and merged (C3a to C3c, N1, N1b, N2, N2b; N2c, the signal handling, is a follow-up); none of it has been run on real machines. Mark FR-C1 and FR-C2 verified only after the steps and the negative checks have passed; do not mark them on a green CI.

**On S**

1. Start the server with the door on a LAN address and the transport on (the transport is what gives the node a pin to check):
   `EMBER_ENROL_LISTEN=<S-lan-ip>:8742 EMBER_ENROL_LISTEN_ALLOW_REMOTE=1 EMBER_ENROL_PUBLIC_URL=http://<S-lan-ip>:8742 EMBER_TRANSPORT=1 ember-server`
   The server logs a warning that the door speaks plain HTTP. It prints `ember server peer id: <64 hex>`; note it. The owner API stays on `127.0.0.1:8740`.
2. Mint a code. The token is in `<data dir>/server-token`:
   `curl -s -X POST http://127.0.0.1:8740/api/computers/enrol-codes -H "Authorization: Bearer $(cat <data dir>/server-token)" -H 'content-type: application/json' -d '{"name":"laptop"}'`
   The answer has `id`, `code`, `command`, `server`, `door`, `server_peer_id` and `server_fingerprint`. Expect `command` to start `ember-node enrol dpe_`, to name `http://<S-lan-ip>:8742` (not port 8740) and to end `--server-fingerprint <the 64 hex id>`.

**On N**

3. Run the command exactly as printed, adding `--url http://<N-lan-ip>:8741` so the server does not have to guess the address (it would otherwise use the source address it sees and port 8741). Expect, in order: the server URL, `TLS: no`, `This node will accept commands from this server.`, the plain-HTTP warning, `Continue? [y/N]`, then `Waiting for the owner to confirm this computer in Ember`.

**On S again**

4. See the pending request: `curl -s http://127.0.0.1:8740/api/computers/enrol-codes/<id> -H "Authorization: Bearer ..."` shows `status: awaiting_confirmation`, `node_info`, `observed_addr` and `fingerprint`. Compare `fingerprint` with the one N printed. Confirm: `curl -s -X POST .../enrol-codes/<id>/confirm -H "Authorization: Bearer ..." -H 'content-type: application/json' -d '{}'`.

**On N again**

5. Within a poll or two N prints `Enrolled as laptop (...)`, the line that it accepts commands from the server's fingerprint over the transport and from the token holder over HTTP, and the SIGHUP hint. Check `~/.ember/node/enrol.json` exists with mode 600 (the token is in it; do not paste it anywhere) and that `~/.ember/node/allowed-peers` contains exactly the server's peer id.
6. Start the node so S can reach it: `EMBER_NODE_LISTEN=0.0.0.0:8741 EMBER_NODE_TRANSPORT=1 ember-node` (no `EMBER_NODE_TOKEN`: it reads `enrol.json`; the log says `node token source` is `enrol.json`).

**Check**

7. On S: `curl -s http://127.0.0.1:8740/api/computers -H "Authorization: Bearer ..."` lists `laptop` and its status becomes online. Open it in the app: files, a terminal, an agent session.

**Negative checks (these are the security properties; each must be run)**

8. Wrong pin: mint a new code and rerun step 3 with one character of `--server-fingerprint` changed. Expect: it aborts after the owner confirms, prints both fingerprints and the computer name, and writes NOTHING (`enrol.json` and `allowed-peers` unchanged). The server has registered the computer anyway: remove it with `curl -s -X DELETE .../api/computers/<computer_id>`.
9. No pin, not interactive: mint, then run the command without `--server-fingerprint` and with `--yes` and the input piped from `/dev/null`. Expect `enrol.json` written, `allowed-peers` NOT touched, and the text that says how to add the id.
10. The code is single use: run the same command a second time with the old code. Expect `this enrol code cannot be used` and no detail.
11. A wrong Host is refused: `curl -s -i http://<S-lan-ip>:8742/enrol/status/x -H 'Host: evil.example'` returns 403; the same request with the right Host returns 404.

**Undo**

12. On S: `curl -s -X DELETE http://127.0.0.1:8740/api/computers/<computer_id> -H "Authorization: Bearer ..."`. On N: delete `enrol.json`, delete the server's line from `allowed-peers`, send the node SIGHUP (it disconnects removed peers).

**What this run does NOT cover, and one gap it will show**

- https in front of the door (a reverse proxy with TLS) and `X-Forwarded-For` behind a proxy: a second run with a proxy.
- A node behind NAT: enrolment registers the computer by the HTTP address (`--url`, or the source address plus port 8741), even when `--transport` was given. The node's peer id is sent and shown, but a computer reached only through the transport (no address S can use) is not registered by peer after an enrol; today that needs `POST /api/computers` with `peer`. This is a finding of the review, recorded as a follow-up, not a defect of the run.
- Plain-HTTP exposure: on a shared LAN the code, the poll secret and the credential cross in clear (the warning says so); run it on a network you trust.

### Not verified

Nothing of the FR-C2 design is built, compiled or run. The unknowns are whether an X25519 crate is already in `Cargo.lock`, whether the node's reqwest has TLS, and whether 8741 is the node port everywhere. The LAN path, a real browser confirm, a node behind NAT, TLS in front of the door, XFF behind a real proxy and sniffing need two real machines and are UNVERIFIED.
