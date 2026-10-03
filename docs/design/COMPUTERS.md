# COMPUTERS.md: computers, switching and tool interception (#6)

> Status: **implemented, not yet run end to end** (2026-10-03). Covers SPEC `FR-X2`, `FR-X3` and
> `FR-S7` v0 on ember server. The mechanism choice is in `INTERCEPTION.md`; this note records
> what was built, what it relies on, and what is still open. **[V]** = verified locally (evidence
> named), **[U]** = unverified.

## What exists

| Piece | Where | What it does |
|-|-|-|
| Registry | `crates/server/src/computers/mod.rs` (`Registry`) | Tables `computers(id, name, url, token, created_at)` and `session_computer(session_id, computer_id, env_json, notice, switched_at)` in `ember.db`, created by store migration 3 (`computers/schema.rs`) and read through the main `Store` connection, like `accounts`. |
| Service | `computers::Computers` | Register / list / probe (`/v1/health`, `/v1/env`) / remove; a session's current computer; `switch`. Installs three hooks on `Sessions`: an instructions hook (environment block), a start-config hook (Codex remote executor / Claude Code shell shim), and a message hook (the FR-S7 notice). |
| HTTP API | `crates/server/src/computers/api.rs` | `GET/POST /api/v1/computers`, `GET/DELETE /api/v1/computers/{id}`, `GET/PUT /api/v1/sessions/{id}/computer`. |
| Codex relay | `crates/server/src/computers/relay.rs` | Loopback WebSocket (`ws://127.0.0.1:<port>/<secret>`) that codex app-server connects to; re-frames to the node's raw `/v1/exec-server` stream. |
| Node bridge | `crates/node/src/exec_server.rs` | `GET /v1/exec-server` (bearer auth) runs `codex exec-server --listen stdio` per connection and relays bytes; `ember-node exec-server` runs the same command on its own stdio. |
| Browser egress | `crates/server/src/computers/egress.rs`, `crates/node/src/egress.rs` | Per computer, a loopback SOCKS5 listener (`socks5://127.0.0.1:<port>`) that a project's browser uses as `--proxy-server`; each connection becomes one node `/v1/egress` stream where the node runs SOCKS5 (FR-R1, see `REMOTE-BROWSER.md`). A computer that is some project's browser egress cannot be removed (409). |
| Claude shim | `crates/server/src/computers/shim.rs`, `crates/server/src/bin/ember-exec.rs` | `CLAUDE_CODE_SHELL_PREFIX` target; runs Bash-tool commands on the node via `/v1/exec` and carries the cwd back. |
| Mount | `crates/server/src/computers/mount.rs` (+ `mount/`) | Mounts a Claude Code session's cwd from its node at the same path: `remote_fs.rs` (node-backed filesystem with caches), `nfs.rs` (loopback NFSv3, feature `mount-nfs`), `fuse.rs` (Linux FUSE, feature `mount-fuse`), `cmd.rs` (mount commands, mount point checks). **Written, not compiled or run**; see § Project mount. |

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
   - the **environment block** built from `/v1/env` as system-level instructions: Claude Code
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
`scripts/ember-mount-setup.sh <path>` (run once with `sudo`) prepares it:
- macOS `/Users/<other>/…`: `mkdir -p` + `chown`.
- macOS `/home/<u>/…`: `/home` is an autofs mount (`auto_home`); comment out its line in
  `/etc/auto_master` and `automount -vc`, then `mkdir` + `chown`.
- macOS new top-level directory (`/srv`, `/workspace`): a line in `/etc/synthetic.conf` and a
  reboot (the system volume is read-only), then `mkdir` + `chown`.
- Linux `/Users/<u>/…` (a Mac node's paths on the Pi) or `/home/<other>/…`: `mkdir -p` + `chown`.

A non-empty local directory at the same path is not mounted over (it would hide it, and a local
session might use it) unless `EMBER_MOUNT_SHADOW=1`. A stale mount from a crashed run is
force-unmounted first.

**Node API added for it** (all under the path policy and the write lock): `/v1/fs/lstat`,
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
`ClientError::Transport` → `EIO`; both count as an outage. Tests: `crates/server/tests/peer_mount.rs`
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

- **Not run end to end.** No real Codex turn through a remote environment and no real Claude
  Bash call through `ember-exec` has been made yet; verify both on the Pi.
- **Peer-addressed mount: not compiled or run.** `NodeClient::with_deadline`, `Computers::node_fs`
  and `crates/server/tests/peer_mount.rs` were written without building. Also open: a timed-out
  request over the transport drops its hyper `SendRequest`, but the background connection task
  of a peer that never answers may keep its stream open until the transport connection ends
  **[U]**; and the dialer keeps its cached connection after a timeout, so a silently dead path
  costs one deadline per request until the transport's own idle timeout closes it and the next
  request re-dials (forgetting the connection on timeout would also cut healthy concurrent
  requests). WebSockets (exec, terminal attach) still have no deadline on either reach.
- **Project mount not compiled or run.** Written against `nfsserve` 0.11.0 and `fuser` 0.18.0
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
  pushing invalidations over `/v1/events` would make it exact.
- **Exclusive rename / hard links / xattrs / locks** are not supported through the mount
  (`RENAME_NOREPLACE` → `EINVAL`, locks local only).
- **Claude cwd tracking**: the shim writes the node's `pwd -P` to the local cwd file; with the
  mount the directory exists here only if it is under the mounted cwd.
- **Token exposure**: the node token sits in the Claude process environment (and so in its local
  hooks' environment). A per-session scoped token is the fix.
- **Codex needs a codex binary on the node** (outside FR-X5's "ember node only"), and the
  exec-server is not confined by the node's path policy.
- **Node protocol**: `/v1/exec-server` and `/v1/egress` were added without bumping `PROTOCOL_VERSION` (additive).
- Codex `additionalContext` (keyed context on `turn/start`) may be a cleaner carrier for the
  environment block than `developerInstructions`; not tried.
- Switch is refused mid-turn; whether a user should be able to force it is open (`INTENT.md` Q2).
