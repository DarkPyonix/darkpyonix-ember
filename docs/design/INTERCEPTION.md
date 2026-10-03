# INTERCEPTION.md: running a wrapped CLI's tools on an ember node

> Status: **design note for #6** (2026-10-03). Nothing here is implemented yet. It answers part
> of `INTENT.md` Q6 for D4/E2. The question is how an unmodified Claude Code or Codex process on
> ember server can have its tool actions (file read, edit and write, glob and grep, shell) carried
> out on the session's current computer through **ember node** (`crates/node/`), while the agent keeps
> its native behaviour.
>
> Facts were checked on 2026-10-03 against **Claude Code 2.1.288** and **codex-cli 0.155.1** on
> macOS. **[V]** marks a fact verified locally, with the evidence named. **[U]** marks a fact that
> is unverified or inferred.

## Where the tools actually run

**Claude Code**

- Bash runs `<shell> -c -l "<snapshot> && eval <cmd> && pwd -P >| <cwd-file>"` as a local child.
  It uses `$SHELL`, which `CLAUDE_CODE_SHELL` overrides. **[V]** (binary strings)
- `CLAUDE_CODE_SHELL_PREFIX` wraps that whole command string, including the cwd-tracking step.
  It also wraps hook commands and stdio MCP launches. **[V]** (binary)
- Grep spawns the ripgrep embedded in the `claude` binary as a local process. **[V]** (`rg` here is
  the claude binary run as `ARGV0=rg`)
- Read, Edit, Write and Glob use the fs module inside the CLI process. **[U]** (strong inference;
  no subprocess was found)
- Built-in tools can be removed with `--tools ""`, `--tools "Bash,Read"` or `--disallowedTools`,
  and replaced through `--mcp-config`. **[V]** (`claude --help`)
- A PreToolUse hook can deny a call or rewrite it (`updatedInput`). The tool still executes
  locally. **[V]** (hook schema in binary) Some modes may turn a rewrite into a permission prompt.
  **[U]**
- `CLAUDE_CODE_REMOTE` and `--remote-control` move the whole CLI or its UI. No option was found
  that sends only tool execution elsewhere. **[V]** (absence in strings is weak evidence)

**Codex**

- `codex exec-server --listen ws://…|stdio` is an **experimental** JSON-RPC executor. **[V]**
  (`codex --help`, strings) Its methods are:
  - `process/start|read|writeStdin|terminate|resizePty`
  - `fs/readFile|writeFile|readDirectory|getMetadata|createDirectory|remove|rename|copy|canonicalize|symlink`
- app-server (experimental API) accepts `environment/add {environmentId, execServerUrl}`. Thread
  and turn params carry `environments: [{environmentId, cwd}]`. `environment/info` returned the
  exec-server's own shell and cwd. **[V]** (live test, no model turn; `generate-ts --experimental`)
- A real turn's shell, unified_exec and apply_patch executing through that environment is **[U]**.
  The fs RPCs make it plausible.
- apply_patch is applied by codex itself (`--codex-run-as-apply-patch`), not by a shell command.
  **[V]** (strings)
- app-server `dynamicTools` lets the client define tools, which come back as `item/tool/call`.
  **[V]** (generated TS)
- Codex has no verified shell-prefix hook. **[U]**
- Whether exec-server ships for Linux arm64 (Raspberry Pi) is **[U]**.

## Options

| | (a) Shell shim + filesystem view | (b) Hooks / MCP tools replace built-ins | (c) Native remote executor |
|-|-|-|-|
| **Mechanism** | Mount the node's project directory on the server **at the same absolute path**. Point the shell prefix at an `ember-exec` shim that runs the command through ember node `/v1/exec`. | Disable the built-ins and inject an MCP server whose Read/Edit/Bash call ember node. Hooks can only deny or rewrite; they cannot execute remotely. | Codex: attach the node as an exec-server environment. Claude Code: no equivalent found. |
| **Native behaviour kept** | Claude: every built-in tool, the read-before-edit tracking, edit diffs, permission UI and transcript shape all stay as they are. Codex: file tools only; its shell still runs on the server. | Little. The model is tuned to the built-in tool schemas. Read-before-edit state, diff cards, background-shell handling and approval semantics would have to be re-implemented, which goes against E2. | Codex: all of it, including the shell, PTY and apply_patch (**[U]** until a turn is tested). |
| **Lost / risky** | cwd tracking: the prefix also wraps `pwd -P >| <tmp>`, so the shim must return that file to the server. Shell snapshot sourcing must work on the node's shell. Paths: `/home/x` on a macOS server needs `synthetic.conf`. | Prompt and tool drift on every CLI release. MCP results render as generic tool calls. | Experimental protocol that may change between Codex releases. A codex binary is needed on the node, outside FR-X5's "ember node only". |
| **Latency** | One round trip per syscall. Kernel attribute caching helps but can serve stale data. Grep walks the mount, which is slow on large trees. Exec costs one round trip. | One round trip per tool call, which is the best case. | One round trip per RPC, about per tool call. |
| **Failure modes** | A hung mount blocks the agent in uninterruptible I/O. The mount must use soft or interruptible options with timeouts. Remount on a computer switch. | A node outage surfaces as tool errors the agent can reason about. | The environment shows "pending" or unreachable. Version skew between server and node codex. |
| **Platforms** | Linux server (Pi): FUSE works. macOS server: macFUSE is not installed and needs a kext or FSKit. The built-in `mount_nfs`, `mount_smbfs` and `mount_webdav` exist **[V]**, so a loopback NFS server needs no kext. FUSE-T works this way **[U]**. | All. | Codex: wherever exec-server builds exist (**[U]** for the Pi). |

## Recommendation

1. **Codex: (c).** ember node supervises `codex exec-server --listen stdio` on the node. It bridges
   that stdio over the node transport as one more stream, next to the HTTP API, which is already
   listener-agnostic. ember server registers it with `environment/add`. First, verify with a real
   turn that the shell and apply_patch execute remotely.
2. **Claude Code: (a).** Set `CLAUDE_CODE_SHELL_PREFIX` to an `ember-exec` shim. The shim calls
   `/v1/exec` with the session's node, cwd and env, and copies the cwd file back. Add a filesystem
   view of the project roots, backed by ember node's fs API: FUSE on Linux servers, a loopback
   NFSv3 server inside ember server on macOS (no kext). It is mounted at the node's own absolute
   paths so FR-X2 holds.
3. **(b) only as a fallback** for an agent with neither hook. Use PreToolUse hooks for guards,
   such as refusing paths outside the project, and not as the executor.
4. Option (a) needs ember node API additions: rename, remove, mkdir, symlink and readlink,
   setattr/chmod, and a batched stat/readdir to cut round trips. It also needs a change
   notification so the mount's cache can be invalidated. Option (c) needs a raw-stream bridge
   endpoint. *(2026-10-03: the fs additions and the mount are written — `COMPUTERS.md` § Project
   mount; the batched stat is covered by listings that carry attributes, and the change feed is
   replaced for now by a 1 s TTL plus an invalidation from `ember-exec` after each command.)*
5. Measure the latency of (a) on the Pi before committing. If the per-syscall cost is too high,
   prefetch the project tree by content hash, which already exists in `/v1/fs/read`.
