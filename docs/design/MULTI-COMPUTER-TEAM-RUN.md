# Multi-computer and multi-account team run (FR-T7, #82)

Status: **server wiring and scripted tests are in place (`ember/server/src/a2a/team.rs`,
`ember/server/tests/a2a_team.rs`, `ember/server/tests/accounts_flow.rs`); real two-computer,
two-account execution is not yet verified and requires the owner.**

Requirement (`docs/SPEC.md` `FR-T7`, `FR-T1`, `FR-X1`..`FR-X3`, `FR-U1`..`FR-U2`, #82): a leader
session spawns teammate sessions across two real computers and two isolated accounts (Claude Code
and Codex), assigns tasks, exchanges team mail and A2A messages, and verifies that each teammate's
tool calls run on its assigned computer and under its assigned account.

---

## 1. What requires the owner's action (`[OWNER ACTION REQUIRED]`)

An agent session on this machine cannot complete #82 alone because three parts need physical
hardware and interactive vendor sign-ins:

1. **`[OWNER ACTION REQUIRED]` Second physical computer (`computer-b`) running `ember-node`**:
   - Start `ember-node` on a second machine (LAN or behind NAT with `EMBER_NODE_TRANSPORT=1`) and
     pair it with the main server using the one-time enrol code (`POST /api/computers/enrol-codes`
     or the app's `+ 컴퓨터 추가` sheet, `ember-node enrol <door-url> <code>`, then confirm in the
     app or via `POST /api/computers/enrol-requests/{id}/confirm`).
   - Verify the project folder exists on both machines (or enable `EMBER_MOUNT` when using
     remote file mounts).
2. **`[OWNER ACTION REQUIRED]` Two isolated accounts logged into the vendor CLIs (`FR-U1`)**:
   - Ember never automates vendor login or copies credentials (`ember/server/src/accounts/mod.rs`).
   - After creating two account slots via `POST /api/accounts` (for example one `claude-code`
     account `cc-acct-1` and one `codex` account `cx-acct-2`, or two accounts per harness), run the
     login command printed in `login.command` of the response on the main server host:
     - Claude Code: `CLAUDE_CONFIG_DIR=<data-dir>/accounts/<id> claude auth login`
     - Codex: `CODEX_HOME=<data-dir>/accounts/<id> codex login`
   - Ensure `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, and `CODEX_API_KEY` are **unset** in the
     `ember-server` process environment so they do not override the per-account directories.
3. **`[OWNER ACTION REQUIRED]` Real model turns**:
   - The run spends a small number of real tokens on Claude Code (`haiku` or `sonnet`) and Codex to
     execute `ember-a2a team spawn`, `task add`, `task update`, and `mail send`.

---

## 2. Architecture of a multi-computer, multi-account team (`FR-T7`)

```
Main server (Computer A: ember-server + ember-shim + ember-a2a + ember-exec)
 ├── Leader session ("lead", Claude Code, account: cc-acct-1, computer: local)
 │     └── runs `ember-a2a team spawn worker-b --agent codex --account <cx-acct-2> --computer <comp-b> "..."`
 └── Teammate session ("worker-b", Codex, account: cx-acct-2, computer: comp-b)
       ├── Process runs on Main server under `ember-shim`, with `CODEX_HOME=<data>/accounts/<cx-acct-2>`
       ├── API egress (`HTTPS_PROXY`) tunnels through `comp-b`'s `/egress` (`FR-X6`)
       └── Tool calls / commands execute on Computer B (`ember-node`) and return results
```

- All agent processes and shims (`ember-shim`) live on the main server (`INTENT.md` D3, `FR-S8`),
  and `ember-a2a` talks to the main server over loopback (`EMBER_URL`, `EMBER_RUNTIME_TOKEN`).
- When the leader passes `--account <account-id>` and `--computer <computer-id>` to
  `ember-a2a team spawn`:
  1. `Sessions::create_with_account` binds the teammate session to `<account-id>` (`FR-U1`/`FR-U2`),
     injecting `CLAUDE_CONFIG_DIR` or `CODEX_HOME` at process start.
  2. `a2a.set_computer_setter` (`ember/server/src/main.rs`) calls
     `Computers::switch(&sessions, &session_id, &computer_id)` (`FR-X3`), placing the teammate's
     tool execution and egress proxy on `<computer-id>`.
  3. The initial prompt is delivered as an A2A message from `lead` (`FR-T3`), waking the teammate.

---

## 3. Step-by-step run procedure

Assume `BASE="http://127.0.0.1:8740"` (or `18740` under `real-server-smoke.sh`) and
`HDR="<data-dir>/secrets/auth.header"` containing `Authorization: Bearer <server-token>`.

### Step 1: Verify binaries next to `ember-server`

`ember-server`, `ember-shim`, `ember-a2a`, and `ember-exec` must sit in the same binary directory:

```sh
(cd ember/server && CARGO_BUILD_JOBS=2 cargo build --release --bins)
(cd ember/node   && CARGO_BUILD_JOBS=2 cargo build --release)
```

### Step 2: Create and sign in two accounts (`[OWNER ACTION REQUIRED]`)

Create one account for Claude Code and one for Codex (or two accounts for the same harness):

```sh
# 1. Create the Claude Code account slot
curl -sS -H @"$HDR" -H 'content-type: application/json' \
  -d '{"agent":"claude-code","label":"team-claude-a"}' \
  "$BASE/api/accounts" | jq .

# 2. Create the Codex account slot
curl -sS -H @"$HDR" -H 'content-type: application/json' \
  -d '{"agent":"codex","label":"team-codex-b"}' \
  "$BASE/api/accounts" | jq .
```

Copy the `id` (`CC_ACCT`, `CX_ACCT`) and run each account's `login.command` in a terminal on the
main server (`[OWNER ACTION REQUIRED]`), then verify both report `logged_in`:

```sh
curl -sS -X POST -H @"$HDR" "$BASE/api/accounts/$CC_ACCT/check" | jq '.status'
curl -sS -X POST -H @"$HDR" "$BASE/api/accounts/$CX_ACCT/check" | jq '.status'
```

### Step 3: Enrol the second computer (`[OWNER ACTION REQUIRED]`)

 ensure `ember-server` is started with `EMBER_ENROL_LISTEN` set (and `EMBER_TRANSPORT=1` if joining
by peer). The door stays closed until a code is minted and closes when no enrolment needs it; set
`EMBER_ENROL_AUTO_CLOSE=0` to keep it open from startup (`FR-C2l`). Mint a one-time code on the main server:

```sh
curl -sS -X POST -H @"$HDR" "$BASE/api/computers/enrol-codes" | jq .
```

On the second computer (`computer-b`), run the printed `ember-node enrol ...` command, then confirm
the join request on the main server (or in the app's `+ 컴퓨터 추가` sheet):

```sh
# List pending enrol requests to get ENROL_ID and fingerprint
curl -sS -H @"$HDR" "$BASE/api/computers/enrol-requests" | jq .

# Confirm the request
curl -sS -X POST -H @"$HDR" -H 'content-type: application/json' \
  -d "{\"expect_fingerprint\":\"$FINGERPRINT\"}" \
  "$BASE/api/computers/enrol-requests/$ENROL_ID/confirm" | jq .
```

Record the new computer's `id` as `COMP_B`, and verify `reachable: true`:

```sh
curl -sS -H @"$HDR" "$BASE/api/computers/$COMP_B" | jq '{id, name, reachable}'
```

### Step 4: Create the project and the leader session on Computer A (`local`)

Create a shared project path `/tmp/ember-team-run` on both Computer A and Computer B, then create
the leader session on `local` with account `$CC_ACCT`:

```sh
curl -sS -H @"$HDR" -H 'content-type: application/json' \
  -d '{"name":"team-verify"}' "$BASE/api/projects" | jq .

curl -sS -H @"$HDR" -H 'content-type: application/json' \
  -d "{\"project\":\"team-verify\",\"agent\":\"claude-code\",\"model\":\"haiku\",\"account\":\"$CC_ACCT\",\"cwd\":\"/tmp/ember-team-run\",\"title\":\"lead\"}" \
  "$BASE/api/sessions" | jq .
```

Record the leader session's `id` as `LEAD_ID`.

### Step 5: Instruct the leader to spawn a teammate on `COMP_B` and `$CX_ACCT`

Send the leader a prompt that spawns `worker` on `--agent codex --account $CX_ACCT --computer $COMP_B`,
creates a task, and waits for the teammate's mail:

```sh
curl -sS -X POST -H @"$HDR" -H 'content-type: application/json' \
  -d "{\"text\":\"Use ember-a2a via Bash to: (1) add a task 'Check hostname on computer B' with 'ember-a2a task add', (2) spawn a teammate named worker with 'ember-a2a team spawn worker --agent codex --account $CX_ACCT --computer $COMP_B -- Check uname -n and hostname, update task 1 to done, and send the hostname to lead with ember-a2a mail send lead <hostname>'. Stop after spawning.\"}" \
  "$BASE/api/sessions/$LEAD_ID/messages"
```

Approve the Bash tool calls (or enable approve-all on both sessions for the test run:
`PUT /api/sessions/$LEAD_ID/approve-all` `{"enabled":true}`).

### Step 6: Verify the completion criteria (`FR-T7`, #82)

Once both turns settle, verify all four invariants via the HTTP API and record the outputs in
`docs/SPEC.md` under `FR-T7`:

1. **Team membership and roles**:
   ```sh
   curl -sS -H @"$HDR" "$BASE/api/teams" | jq .
   ```
   - One team exists in project `team-verify` with `lead` (`role: "leader"`, `agent: "claude-code"`)
     and `worker` (`role: "teammate"`, `agent: "codex"`).
2. **Account isolation (`FR-U1`, `FR-U2`)**:
   ```sh
   curl -sS -H @"$HDR" "$BASE/api/sessions/$LEAD_ID" | jq '.session | {id, agent, account_id, account_reason}'
   curl -sS -H @"$HDR" "$BASE/api/sessions/$WORKER_ID" | jq '.session | {id, agent, account_id, account_reason}'
   ```
   - `lead` has `account_id == $CC_ACCT`; `worker` has `account_id == $CX_ACCT`.
3. **Computer placement (`FR-X1`..`FR-X3`)**:
   ```sh
   curl -sS -H @"$HDR" "$BASE/api/sessions/$WORKER_ID/computer" | jq .
   ```
   - `worker` reports `computer_id == $COMP_B`, and the hostname reported in its transcript / mail
     is Computer B's hostname, not Computer A's.
4. **Shared task list and mailbox (`FR-T7`)**:
   - Task `#1` in the team's task list has `status: "done"`.
   - The team mailbox contains the message from `worker` to `lead` with Computer B's hostname, and
     `lead`'s transcript shows the delivered `[ember a2a]` mail.
