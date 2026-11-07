#!/usr/bin/env bash
# Real server mode smoke harness: ember-server + one ember-node + ember-app on loopback, with every
# file under <repo>/.scratch/real-server-smoke/, never ~/.ember or ~/Library/Application Support/ember.
# Design and manual steps: docs/design/REAL-SERVER-RUN.md.
#
#   real-server-smoke.sh check            non-interactive API checks (no model call), exit 0 or 1
#   real-server-smoke.sh up               start server, node and app; seed a project and sessions
#   real-server-smoke.sh steps            print the manual steps again
#   real-server-smoke.sh status           processes, computers, sessions (no secrets)
#   real-server-smoke.sh app              restart the app only (the app restart check)
#   real-server-smoke.sh server-restart   restart the server only, keeping its data
#   real-server-smoke.sh server-update    update/replace the server while sessions run, checking shim survival (FR-S8)
#   real-server-smoke.sh down             stop everything `up` started, agents included
#   real-server-smoke.sh reset            `down`, then delete the live run directory
#
# Exit codes: 0 ok, 1 a check failed, 2 usage, 3 a binary is missing (the build commands are
# printed), 4 a port is held by a process this script did not start.
#
# Secrets: the server token and the node token stay in 0600 files under the run directory. They are
# passed to curl as a header file (`-H @file`) and to jq with `--rawfile`, so they are never on a
# command line this script runs, and never printed.
set -euo pipefail

D="$(cd "$(dirname "$0")" && pwd)"
R="$(cd "$D/../../.." && pwd)"
SMOKE_DIR="${SMOKE_DIR:-$R/.scratch/real-server-smoke}"
MODEL="${SMOKE_CLAUDE_MODEL:-haiku}"
PROJECT_LIVE="smoke-real"
PROJECT_CHECK="smoke-check"

die() { echo "real-server-smoke: $*" >&2; exit 1; }
say() { echo "real-server-smoke: $*"; }

for tool in curl jq lsof; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool is required"
done
jq -n --rawfile x /dev/null '1' >/dev/null 2>&1 || die "jq 1.6 or newer is required (--rawfile)"

# The run directory must stay inside the repository (AGENTS.md, "Where files go").
case "$SMOKE_DIR" in
  "$R"/*) ;;
  *) die "SMOKE_DIR must be inside $R" ;;
esac

# ---------------------------------------------------------------------------------------------
# Binaries

mtime() { stat -f %m "$1" 2>/dev/null || stat -c %Y "$1" 2>/dev/null || echo 0; }

# Every checkout of this repository: the main one and its worktrees.
checkouts() {
  echo "$R"
  git -C "$R" worktree list --porcelain 2>/dev/null | sed -n 's/^worktree //p'
}

# find_bin <binary> <crate dir>: the newest release build (debug too with SMOKE_ALLOW_DEBUG=1).
find_bin() {
  local bin="$1" crate="$2" best="" best_t=0 c p t profiles="release"
  [ "${SMOKE_ALLOW_DEBUG:-0}" = "1" ] && profiles="release debug"
  while IFS= read -r c; do
    for prof in $profiles; do
      p="$c/ember/$crate/target/$prof/$bin"
      [ -x "$p" ] && [ -f "$p" ] || continue
      t="$(mtime "$p")"
      if [ "$t" -gt "$best_t" ]; then best="$p"; best_t="$t"; fi
    done
  done < <(checkouts | awk '!seen[$0]++')
  echo "$best"
}

build_help() {
  cat >&2 <<EOF

Build them once (one build at a time, AGENTS.md), from $R:

  (cd ember/server && CARGO_BUILD_JOBS=2 cargo build --release --bins)   # ember-server, ember-shim, ember-a2a, ember-exec
  (cd ember/node   && CARGO_BUILD_JOBS=2 cargo build --release)          # ember-node
  (cd ember/app    && CARGO_BUILD_JOBS=2 cargo build --release)          # ember-app

Or point at existing binaries: EMBER_SERVER_BIN, EMBER_NODE_BIN, EMBER_APP_BIN.
SMOKE_ALLOW_DEBUG=1 also accepts target/debug builds.
EOF
}

# resolve_bins [app]: sets SERVER_BIN, NODE_BIN and, with "app", APP_BIN.
resolve_bins() {
  local missing=""
  SERVER_BIN="${EMBER_SERVER_BIN:-$(find_bin ember-server server)}"
  NODE_BIN="${EMBER_NODE_BIN:-$(find_bin ember-node node)}"
  [ -n "$SERVER_BIN" ] || missing="$missing ember-server"
  [ -n "$NODE_BIN" ] || missing="$missing ember-node"
  if [ "${1:-}" = "app" ]; then
    APP_BIN="${EMBER_APP_BIN:-$(find_bin ember-app app)}"
    [ -n "$APP_BIN" ] || missing="$missing ember-app"
  fi
  if [ -n "$missing" ]; then
    echo "real-server-smoke: no build found for:$missing" >&2
    build_help
    exit 3
  fi
  local sdir; sdir="$(dirname "$SERVER_BIN")"
  say "ember-server: $SERVER_BIN"
  say "ember-node:   $NODE_BIN"
  [ "${1:-}" = "app" ] && say "ember-app:    $APP_BIN"
  [ -x "$sdir/ember-shim" ] || say "warning: no ember-shim next to ember-server; agents stop with the server (FR-S8 off)"
  [ -x "$sdir/ember-a2a" ] || say "warning: no ember-a2a next to ember-server; real agents cannot send A2A replies or channel posts"
  # Binaries from different checkouts may disagree on the API.
  local a b; a="$(dirname "$(dirname "$(dirname "$SERVER_BIN")")")"; b="$(dirname "$(dirname "$(dirname "$NODE_BIN")")")"
  [ "$(dirname "$a")" = "$(dirname "$b")" ] || say "warning: ember-server and ember-node come from different checkouts"
  return 0
}

# ---------------------------------------------------------------------------------------------
# Processes

# set_run <dir> <server port> <node port>
set_run() {
  RUN="$1"; PORT="$2"; NODE_PORT="$3"
  BASE="http://127.0.0.1:$PORT"
  SD="$RUN/server"; ND="$RUN/node"; AD="$RUN/app"; WORK="$RUN/work"; SEC="$RUN/secrets"; LOGS="$RUN/logs"
  HDR="$SEC/auth.header"
}

mkdirs() {
  mkdir -p "$SD" "$ND" "$AD" "$WORK" "$LOGS" "$RUN/tmp"
  (umask 077; mkdir -p "$SEC")
  chmod 700 "$SEC" "$SD" "$AD"
}

alive() { [ -f "$1" ] && kill -0 "$(cat "$1")" 2>/dev/null; }

# stop_pid <pid file> <name>
stop_pid() {
  local f="$1" name="$2" pid
  alive "$f" || { rm -f "$f"; return 0; }
  pid="$(cat "$f")"
  kill "$pid" 2>/dev/null || true
  for _ in $(seq 1 50); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
  if kill -0 "$pid" 2>/dev/null; then kill -9 "$pid" 2>/dev/null || true; fi
  rm -f "$f"
  say "stopped $name (pid $pid)"
}

# port_free <port> <our pid file>: fails when another process listens on the port.
port_free() {
  local port="$1" f="$2" holder
  holder="$(lsof -nP -iTCP:"$port" -sTCP:LISTEN -t 2>/dev/null | head -1 || true)"
  [ -z "$holder" ] && return 0
  if [ -f "$f" ] && [ "$holder" = "$(cat "$f")" ]; then return 0; fi
  echo "real-server-smoke: port $port is held by pid $holder ($(ps -p "$holder" -o comm= 2>/dev/null || echo '?')), not by this script." >&2
  echo "  Pick other ports with SMOKE_PORT / SMOKE_NODE_PORT (check mode: SMOKE_CHECK_PORT / SMOKE_CHECK_NODE_PORT)." >&2
  exit 4
}

wait_http() {
  local url="$1" what="$2"
  for _ in $(seq 1 100); do
    curl -fsS -o /dev/null "$url" 2>/dev/null && return 0
    sleep 0.1
  done
  die "$what did not answer at $url; see $LOGS"
}

start_server() {
  alive "$RUN/server.pid" && { say "ember-server already running (pid $(cat "$RUN/server.pid"))"; return 0; }
  port_free "$PORT" "$RUN/server.pid"
  # Isolation: own data dir and TMPDIR (private MCP config files), no hub, no relay roles, no
  # transport, no guest door, no demo data. EMBER_SCRIPTED_AGENT=1 offers the no-model test agent.
  local extra_env=()
  [ -n "${SMOKE_CLAUDE_BIN:-}" ] && extra_env+=("EMBER_CLAUDE_BIN=$SMOKE_CLAUDE_BIN")
  env -u EMBER_DEMO_DATA -u EMBER_TRANSPORT -u EMBER_GUEST_LISTEN -u EMBER_SERVER_TOKEN \
    -u EMBER_ACP_AGENTS -u EMBER_ACP_AGENTS_FILE -u EMBER_HOSTED \
    "${extra_env[@]}" \
    EMBER_DATA_DIR="$SD" EMBER_LISTEN="127.0.0.1:$PORT" EMBER_SCRIPTED_AGENT=1 \
    EMBER_HUB_URL=off EMBER_SERVER_RELAY=off TMPDIR="$RUN/tmp" RUST_LOG="${RUST_LOG:-info}" \
    nohup "$SERVER_BIN" >>"$LOGS/server.log" 2>&1 &
  echo $! >"$RUN/server.pid"
  wait_http "$BASE/api/health" "ember-server"
  for _ in $(seq 1 50); do [ -s "$SD/server-token" ] && break; sleep 0.1; done
  [ -s "$SD/server-token" ] || die "the server wrote no $SD/server-token"
  # The curl header file and the app's copy of the token, both 0600, written without echoing.
  (umask 077; { printf 'Authorization: Bearer '; head -n 1 "$SD/server-token"; } >"$HDR")
  (umask 077; head -n 1 "$SD/server-token" >"$AD/server-token"); chmod 600 "$AD/server-token"
  say "ember-server on $BASE (pid $(cat "$RUN/server.pid"), log $LOGS/server.log)"
}

start_node() {
  alive "$RUN/node.pid" && { say "ember-node already running (pid $(cat "$RUN/node.pid"))"; return 0; }
  port_free "$NODE_PORT" "$RUN/node.pid"
  [ -s "$SEC/node-token" ] || (umask 077; od -An -tx1 -N32 /dev/urandom | tr -d ' \n' >"$SEC/node-token")
  # The node reads its token from the environment (EMBER_NODE_TOKEN); it is set for that one process.
  env -u EMBER_NODE_TRANSPORT -u EMBER_NODE_SOCKS_LISTEN \
    EMBER_NODE_TOKEN="$(cat "$SEC/node-token")" EMBER_NODE_LISTEN="127.0.0.1:$NODE_PORT" \
    EMBER_NODE_ROOTS="$WORK" EMBER_NODE_STATE_DIR="$ND" EMBER_NODE_KEEP_PTY=0 EMBER_HUB_URL=off \
    TMPDIR="$RUN/tmp" RUST_LOG="${RUST_LOG:-info}" \
    nohup "$NODE_BIN" >>"$LOGS/node.log" 2>&1 &
  echo $! >"$RUN/node.pid"
  wait_http "http://127.0.0.1:$NODE_PORT/health" "ember-node"
  say "ember-node on 127.0.0.1:$NODE_PORT (pid $(cat "$RUN/node.pid"), roots $WORK, log $LOGS/node.log)"
}

start_app() {
  stop_pid "$RUN/app.pid" ember-app
  # Real server mode: EMBER_SERVER_URL set, EMBER_DEMO_DATA unset. EMBER_DATA_DIR keeps the app's
  # cache, prefs and token copy here instead of ~/Library/Application Support/ember.
  env -u EMBER_DEMO_DATA -u EMBER_DEMO_OSE -u EMBER_SERVER_TOKEN \
    EMBER_SERVER_URL="$BASE" EMBER_DATA_DIR="$AD" TMPDIR="$RUN/tmp" \
    nohup "$APP_BIN" >>"$LOGS/app.log" 2>&1 &
  echo $! >"$RUN/app.pid"
  sleep 2
  alive "$RUN/app.pid" || { tail -20 "$LOGS/app.log" >&2; die "ember-app exited at start; see $LOGS/app.log"; }
  say "ember-app pointed at $BASE (pid $(cat "$RUN/app.pid"), data $AD, log $LOGS/app.log)"
}

# Session shims (FR-S8) outlive the server on purpose; ours are found by their --dir.
stop_shims() {
  local pids p
  pids="$(pgrep -f -- "--dir $SD/shims" 2>/dev/null || true)"
  [ -n "$pids" ] || return 0
  for p in $pids; do
    pkill -TERM -P "$p" 2>/dev/null || true
    kill "$p" 2>/dev/null || true
  done
  say "stopped session shims and their agents: $(echo $pids)"
}

stop_all() {
  stop_pid "$RUN/app.pid" ember-app
  stop_pid "$RUN/server.pid" ember-server
  stop_shims
  stop_pid "$RUN/node.pid" ember-node
}

# ---------------------------------------------------------------------------------------------
# HTTP

# api <method> <path> [json body]: sets CODE; the body is in $RES. Never prints.
api() {
  local m="$1" p="$2" b="${3:-}"
  RES="$RUN/tmp/res.json"
  if [ -n "$b" ]; then
    CODE="$(curl -sS -o "$RES" -w '%{http_code}' -X "$m" -H @"$HDR" -H 'content-type: application/json' --data-binary "$b" "$BASE$p" || echo 000)"
  else
    CODE="$(curl -sS -o "$RES" -w '%{http_code}' -X "$m" -H @"$HDR" "$BASE$p" || echo 000)"
  fi
}

# api_file <method> <path> <body file>: the same, with the body read from a (0600) file.
api_file() {
  RES="$RUN/tmp/res.json"
  CODE="$(curl -sS -o "$RES" -w '%{http_code}' -X "$1" -H @"$HDR" -H 'content-type: application/json' --data-binary @"$3" "$BASE$2" || echo 000)"
}

# j <jq args>: read the last response; a body that is not JSON reads as empty.
j() { jq -r "$@" "$RES" 2>/dev/null || true; }

register_node() {
  # The body holds the node token: built by jq from the token file into a 0600 file.
  (umask 077; jq -n --rawfile t "$SEC/node-token" --arg url "http://127.0.0.1:$NODE_PORT" \
    '{name: "smoke-node", url: $url, token: ($t | rtrimstr("\n"))}' >"$SEC/register.json")
  api_file POST /api/computers "$SEC/register.json"
  rm -f "$SEC/register.json"
}

# A session's status and its events.
status_of() { api GET "/api/sessions/$1"; j '.session.status'; }
events_of() { api GET "/api/sessions/$1/events?after=0"; cp "$RES" "$RUN/tmp/events-$1.json"; echo "$RUN/tmp/events-$1.json"; }

# wait_status <id> <status> [tries of 0.1 s]
wait_status() {
  local s
  for _ in $(seq 1 "${3:-150}"); do
    s="$(status_of "$1")"
    [ "$s" = "$2" ] && return 0
    sleep 0.1
  done
  echo "    session $1 is '$s', expected '$2'" >&2
  return 1
}

# settle <id>: answer the session's open approvals until it is no longer mid-turn.
settle() {
  local s a
  for _ in $(seq 1 100); do
    s="$(status_of "$1")"
    case "$s" in
      waiting_for_approval)
        a="$(jq -r '[.[] | .event | select(.kind == "approval_requested") | .approval_id] | last // empty' "$(events_of "$1")")"
        [ -n "$a" ] && api POST "/api/sessions/$1/approvals/$a" '{"decision":"deny"}'
        ;;
      running) ;;
      *) return 0 ;;
    esac
    sleep 0.1
  done
  return 1
}

# ---------------------------------------------------------------------------------------------
# check: no model call, own run directory and ports

PASS=0; FAIL=0
ok()   { PASS=$((PASS + 1)); echo "  PASS  $1"; }
bad()  { FAIL=$((FAIL + 1)); echo "  FAIL  $1"; [ -n "${2:-}" ] && echo "        $2"; return 0; }
# expect <label> <wanted http code(s), | separated>
expect() {
  case "|$2|" in
    *"|$CODE|"*) ok "$1" ;;
    *) bad "$1" "HTTP $CODE, wanted $2: $(head -c 300 "$RES" 2>/dev/null | tr '\n' ' ')" ;;
  esac
}

cmd_check() {
  set_run "$SMOKE_DIR/check" "${SMOKE_CHECK_PORT:-18750}" "${SMOKE_CHECK_NODE_PORT:-18751}"
  resolve_bins
  # A fresh run each time; a previous run's processes are stopped first.
  if [ -d "$RUN" ]; then stop_all >/dev/null 2>&1 || true; rm -rf "$RUN"; fi
  mkdirs
  trap 'stop_all >/dev/null 2>&1 || true' EXIT
  start_server
  start_node
  local P="$PROJECT_CHECK" A B C CID n before after sid
  mkdir -p "$WORK/$P"; printf '# smoke check\n' >"$WORK/$P/README.md"
  echo
  echo "Checks against $BASE (run dir $RUN)"

  # FR-S9: health is open, everything else needs the token.
  CODE="$(curl -sS -o /dev/null -w '%{http_code}' "$BASE/api/health" || echo 000)"
  [ "$CODE" = 200 ] && ok "FR-S9 health answers without a token" || bad "FR-S9 health answers without a token" "HTTP $CODE"
  CODE="$(curl -sS -o /dev/null -w '%{http_code}' "$BASE/api/sessions" || echo 000)"
  [ "$CODE" = 401 ] && ok "FR-S9 no token is refused (401)" || bad "FR-S9 no token is refused (401)" "HTTP $CODE"
  CODE="$(curl -sS -o /dev/null -w '%{http_code}' -H 'Authorization: Bearer not-the-token' "$BASE/api/sessions" || echo 000)"
  [ "$CODE" = 401 ] && ok "FR-S9 a wrong token is refused (401)" || bad "FR-S9 a wrong token is refused (401)" "HTTP $CODE"
  api GET /api/sessions; expect "FR-S9 the token file's token is accepted" 200
  [ "$(stat -f %Lp "$SD/server-token" 2>/dev/null || stat -c %a "$SD/server-token")" = 600 ] \
    && ok "FR-S9 server-token is mode 0600" || bad "FR-S9 server-token is mode 0600"

  # FR-A6: agents are detected (the scripted test agent is not listed; creating its sessions below
  # shows it is offered).
  api GET /api/agents; expect "FR-A6 agent list" 200
  echo "        installed here: $(j '[.[] | select(.installed) | "\(.kind) \(.version // "")"] | join(", ")')"

  # FR-X1: the node is registered and reachable.
  # Only the status is shown for this call: its request carried the node token.
  register_node
  [ "$CODE" = 201 ] && ok "FR-X1 register ember-node as a computer" || bad "FR-X1 register ember-node as a computer" "HTTP $CODE"
  local COMP; COMP="$(j '.id // empty')"
  if [ -n "$COMP" ]; then
    api GET "/api/computers/$COMP"; expect "FR-X1 computer status" 200
    [ "$(j '.reachable')" = true ] && ok "FR-X1 the node is reachable" || bad "FR-X1 the node is reachable" "reachable=$(j '.reachable') error=$(j '.error // empty' | head -c 200)"
  fi

  # FR-L1 / FR-S1: project and sessions.
  api POST /api/projects "{\"name\":\"$P\"}"; expect "FR-L1 create project $P" "201|200"
  api POST /api/sessions "{\"project\":\"$P\",\"agent\":\"scripted\",\"cwd\":\"$WORK/$P\",\"title\":\"alpha\"}"
  expect "FR-S1 create session alpha (scripted)" 201; A="$(j '.id')"
  api POST /api/sessions "{\"project\":\"$P\",\"agent\":\"scripted\",\"cwd\":\"$WORK/$P\",\"title\":\"beta\"}"
  expect "FR-S1 create session beta (scripted)" 201; B="$(j '.id')"
  api GET "/api/sessions?project=$P"
  n="$(j --arg a "$A" --arg b "$B" '[.[] | select(.id == $a or .id == $b)] | length')"
  [ "$n" = 2 ] && ok "FR-L2 both sessions appear in the project's list" || bad "FR-L2 both sessions appear in the project's list" "found $n"

  # FR-S2 / FR-A5: a turn streams into the transcript, stops for an approval, and finishes when answered.
  api POST "/api/sessions/$A/messages" '{"text":"hello smoke"}'; expect "FR-S2 send a message to alpha" 202
  if wait_status "$A" waiting_for_approval; then ok "FR-A5 alpha waits for an approval"; else bad "FR-A5 alpha waits for an approval"; fi
  local EV; EV="$(events_of "$A")"
  local APR; APR="$(jq -r '[.[] | .event | select(.kind == "approval_requested") | .approval_id] | last // empty' "$EV")"
  [ -n "$APR" ] && ok "FR-A5 the approval request is in the transcript ($APR)" || bad "FR-A5 the approval request is in the transcript"
  jq -e '[.[] | .event | select(.kind == "user_message" and .text == "hello smoke")] | length == 1' "$EV" >/dev/null \
    && ok "FR-S2 the user message is stored" || bad "FR-S2 the user message is stored"
  api POST "/api/sessions/$A/approvals/$APR" '{"decision":"allow_once"}'; expect "FR-A5 answer the approval (allow once)" 202
  if wait_status "$A" finished; then ok "FR-A5 the turn finishes after the answer"; else bad "FR-A5 the turn finishes after the answer"; fi
  EV="$(events_of "$A")"
  jq -e '[.[] | .event | select(.kind == "assistant_message" and .text == "echo: hello smoke")] | length == 1' "$EV" >/dev/null \
    && ok "FR-S2 the assistant reply is in the transcript" || bad "FR-S2 the assistant reply is in the transcript"
  jq -e '.[-1].event.kind == "turn_ended" and .[-1].event.outcome == "completed"' "$EV" >/dev/null \
    && ok "FR-S2 the transcript ends with turn_ended completed" || bad "FR-S2 the transcript ends with turn_ended completed" "$(jq -c '.[-1].event' "$EV" | head -c 200)"
  jq -e '[.[].seq] == ([range(1; length + 1)])' "$EV" >/dev/null \
    && ok "FR-S2 event sequence numbers are consecutive" || bad "FR-S2 event sequence numbers are consecutive"

  # FR-T6 / FR-T3: a mention in alpha's composer reaches beta as an A2A message.
  api GET /api/a2a/settings
  [ "$(j '.enabled')" = true ] && ok "FR-T6 A2A is on" || bad "FR-T6 A2A is on" "HTTP $CODE enabled=$(j '.enabled')"
  api POST "/api/sessions/$A/messages" '{"text":"please look @@beta"}'; expect "FR-T6 send a message mentioning @@beta" 202
  local got=""
  for _ in $(seq 1 100); do
    got="$(jq -r '[.[] | .event | select(.kind == "user_message" and (.text | contains("[ember a2a]")) and (.text | contains("please look")))] | length' "$(events_of "$B")")"
    [ "$got" -ge 1 ] 2>/dev/null && break
    sleep 0.1
  done
  [ "${got:-0}" -ge 1 ] 2>/dev/null && ok "FR-T3 beta received the A2A message with the [ember a2a] header" \
    || bad "FR-T3 beta received the A2A message with the [ember a2a] header" "$(jq -c '[.[] | .event | select(.kind == "notice" or .kind == "user_message")] | .[-3:]' "$(events_of "$A")" | head -c 400)"
  settle "$A" && settle "$B" && ok "A2A turns settled (approvals denied)" || bad "A2A turns settled (approvals denied)"

  # FR-T10 / FR-T12: a channel with both agents; a mention is delivered, a plain message is not.
  api POST /api/channels "{\"project\":\"$P\",\"name\":\"smoke-room\",\"agents\":[\"$A\",\"$B\"]}"
  expect "FR-T10 create channel smoke-room with alpha and beta" 201; CID="$(j '.id')"
  before="$(jq -r '[.[] | .event | select(.kind == "user_message")] | length' "$(events_of "$B")")"
  api POST "/api/channels/$CID/messages" '{"text":"people only, no mention"}'; expect "FR-T12 post without a mention" 201
  api POST "/api/channels/$CID/messages" '{"text":"@beta channel ping"}'; expect "FR-T12 post mentioning @beta" 201
  api GET "/api/channels/$CID/messages"
  n="$(j '[.[] | select(.text == "people only, no mention" or .text == "@beta channel ping")] | length')"
  [ "$n" = 2 ] && ok "FR-T10 both posts are in the channel log" || bad "FR-T10 both posts are in the channel log" "found $n"
  got=""
  for _ in $(seq 1 100); do
    got="$(jq -r '[.[] | .event | select(.kind == "user_message" and (.text | contains("channel ping")))] | length' "$(events_of "$B")")"
    [ "$got" -ge 1 ] 2>/dev/null && break
    sleep 0.1
  done
  [ "${got:-0}" -ge 1 ] 2>/dev/null && ok "FR-T12 the mention reached beta's session" || bad "FR-T12 the mention reached beta's session"
  after="$(jq -r '[.[] | .event | select(.kind == "user_message")] | length' "$(events_of "$B")")"
  [ "$after" = $((before + 1)) ] && ok "FR-T12 the post without a mention reached no agent" \
    || bad "FR-T12 the post without a mention reached no agent" "beta user messages $before -> $after"
  settle "$B" || true

  # FR-S2 / FR-S3: the sessions and transcripts survive a server restart, and alpha resumes.
  before="$(jq 'length' "$(events_of "$A")")"
  stop_pid "$RUN/server.pid" ember-server >/dev/null
  start_server >/dev/null
  api GET "/api/sessions?project=$P"
  n="$(j --arg a "$A" --arg b "$B" '[.[] | select(.id == $a or .id == $b)] | length')"
  [ "$n" = 2 ] && ok "FR-S2 both sessions are listed after a server restart" || bad "FR-S2 both sessions are listed after a server restart" "found $n"
  after="$(jq 'length' "$(events_of "$A")")"
  [ "$after" = "$before" ] && ok "FR-S2 alpha's transcript is intact after the restart ($after events)" \
    || bad "FR-S2 alpha's transcript is intact after the restart" "$before -> $after events"
  api POST "/api/sessions/$A/messages" '{"text":"after restart"}'; expect "FR-S2 alpha accepts a message after the restart" 202
  if wait_status "$A" waiting_for_approval; then ok "FR-S2 alpha resumes and runs a turn after the restart"; else bad "FR-S2 alpha resumes and runs a turn after the restart"; fi
  settle "$A" || true

  # FR-S8 (#128): live agent processes under session shims survive a server crash/update/restart.
  # Three Claude Code sessions run concurrently under ember-shim; the server is stopped mid-turn and
  # started again (with the updated binary when EMBER_SERVER_UPDATE_BIN is set); none of the shims or
  # agents stop, all three sessions are rejoined, and every turn completes without losing ticks.
  local sdir; sdir="$(dirname "$SERVER_BIN")"
  if [ -x "$sdir/ember-shim" ]; then
    SMOKE_CLAUDE_BIN="$R/ember/server/tests/fixtures/claude/fake-claude-ticks.sh"
    stop_pid "$RUN/server.pid" ember-server >/dev/null
    start_server >/dev/null
    local S1 S2 S3 m pid1 pid2 pid3
    mkdir -p "$WORK/$P/s1" "$WORK/$P/s2" "$WORK/$P/s3"
    api POST /api/sessions "{\"project\":\"$P\",\"agent\":\"claude-code\",\"cwd\":\"$WORK/$P/s1\",\"title\":\"s8-one\"}"
    expect "FR-S8 create shim session s8-one" 201; S1="$(j '.id')"
    api POST /api/sessions "{\"project\":\"$P\",\"agent\":\"claude-code\",\"cwd\":\"$WORK/$P/s2\",\"title\":\"s8-two\"}"
    expect "FR-S8 create shim session s8-two" 201; S2="$(j '.id')"
    api POST /api/sessions "{\"project\":\"$P\",\"agent\":\"claude-code\",\"cwd\":\"$WORK/$P/s3\",\"title\":\"s8-three\"}"
    expect "FR-S8 create shim session s8-three" 201; S3="$(j '.id')"
    for sid in "$S1" "$S2" "$S3"; do
      api POST "/api/sessions/$sid/messages" '{"text":"go"}'
    done
    local ticks_ready=0
    for _ in $(seq 1 100); do
      if [ "$(jq '[.[] | select(.event.kind == "assistant_message")] | length' "$(events_of "$S1")")" -ge 3 ] \
        && [ "$(jq '[.[] | select(.event.kind == "assistant_message")] | length' "$(events_of "$S2")")" -ge 3 ] \
        && [ "$(jq '[.[] | select(.event.kind == "assistant_message")] | length' "$(events_of "$S3")")" -ge 3 ]; then
        ticks_ready=1
        break
      fi
      sleep 0.1
    done
    [ "$ticks_ready" = 1 ] && ok "FR-S8 all 3 shim sessions emit mid-turn ticks before server update" \
      || bad "FR-S8 all 3 shim sessions emit mid-turn ticks before server update"
    pid1="$(jq -r '.agent_pid // empty' "$SD/shims/$S1.json" 2>/dev/null || true)"
    pid2="$(jq -r '.agent_pid // empty' "$SD/shims/$S2.json" 2>/dev/null || true)"
    pid3="$(jq -r '.agent_pid // empty' "$SD/shims/$S3.json" 2>/dev/null || true)"
    # Stop the server mid-turn (simulating a server update / restart) and verify shims + agents stay alive.
    stop_pid "$RUN/server.pid" ember-server >/dev/null
    if [ -n "$pid1" ] && [ -n "$pid2" ] && [ -n "$pid3" ] \
      && kill -0 "$pid1" 2>/dev/null && kill -0 "$pid2" 2>/dev/null && kill -0 "$pid3" 2>/dev/null; then
      ok "FR-S8 all 3 agent processes keep running while the server is down for update"
    else
      bad "FR-S8 all 3 agent processes keep running while the server is down for update" "pids: $pid1 $pid2 $pid3"
    fi
    [ -n "${EMBER_SERVER_UPDATE_BIN:-}" ] && SERVER_BIN="$EMBER_SERVER_UPDATE_BIN"
    start_server >/dev/null
    local all_done=1
    for sid in "$S1" "$S2" "$S3"; do
      wait_status "$sid" finished 150 || all_done=0
    done
    [ "$all_done" = 1 ] && ok "FR-S8 updated server rejoins all 3 sessions and finishes their turns" \
      || bad "FR-S8 updated server rejoins all 3 sessions and finishes their turns"
    local no_loss=1
    for sid in "$S1" "$S2" "$S3"; do
      if ! jq -e '[.[] | .event | select(.kind == "assistant_message") | .text | sub("^tick ";"") | tonumber] | unique == ([range(1; 31)])' "$(events_of "$sid")" >/dev/null 2>&1; then
        no_loss=0
      fi
    done
    [ "$no_loss" = 1 ] && ok "FR-S8 all 30 ticks per session are stored in order without loss across server update" \
      || bad "FR-S8 all 30 ticks per session are stored in order without loss across server update"
    unset SMOKE_CLAUDE_BIN
  else
    bad "FR-S8 ember-shim binary next to ember-server" "missing $sdir/ember-shim"
  fi

  # FR-L4: the project can be assigned to the node computer (last, so the turns above run locally).
  if [ -n "${COMP:-}" ]; then
    api PUT "/api/projects/$P/computers/$COMP"; expect "FR-L4 assign $P to smoke-node" "200|201|204"
    api GET "/api/projects/$P"
    j 'tostring' | grep -q "$COMP" && ok "FR-L4 the project lists smoke-node" || bad "FR-L4 the project lists smoke-node"
  fi

  echo
  echo "Result: $PASS passed, $FAIL failed (logs: $LOGS)"
  [ "$FAIL" = 0 ]
}

# ---------------------------------------------------------------------------------------------
# up: the interactive run

live_run() { set_run "$SMOKE_DIR/live" "${SMOKE_PORT:-18740}" "${SMOKE_NODE_PORT:-18741}"; }

seed_live() {
  local P="$PROJECT_LIVE" A B C
  [ -f "$RUN/seeded" ] && { say "already seeded (project $P)"; return 0; }
  mkdir -p "$WORK/$P"
  [ -f "$WORK/$P/README.md" ] || printf '# smoke-real\n\nA throwaway project for the real server smoke run.\n' >"$WORK/$P/README.md"
  register_node
  case "$CODE" in 201) say "registered smoke-node";; *) say "warning: registering smoke-node gave HTTP $CODE";; esac
  api POST /api/projects "{\"name\":\"$P\"}"
  case "$CODE" in 200|201) ;; *) die "creating project $P gave HTTP $CODE: $(head -c 300 "$RES")";; esac
  api POST /api/sessions "{\"project\":\"$P\",\"agent\":\"claude-code\",\"model\":\"$MODEL\",\"cwd\":\"$WORK/$P\",\"title\":\"alpha\"}"
  [ "$CODE" = 201 ] || die "creating session alpha gave HTTP $CODE: $(head -c 300 "$RES")"; A="$(j '.id')"
  api POST /api/sessions "{\"project\":\"$P\",\"agent\":\"claude-code\",\"model\":\"$MODEL\",\"cwd\":\"$WORK/$P\",\"title\":\"beta\"}"
  [ "$CODE" = 201 ] || die "creating session beta gave HTTP $CODE: $(head -c 300 "$RES")"; B="$(j '.id')"
  api POST /api/sessions "{\"project\":\"$P\",\"agent\":\"scripted\",\"cwd\":\"$WORK/$P\",\"title\":\"gamma\"}"
  [ "$CODE" = 201 ] || die "creating session gamma gave HTTP $CODE: $(head -c 300 "$RES")"; C="$(j '.id')"
  api POST /api/channels "{\"project\":\"$P\",\"name\":\"smoke-room\",\"agents\":[\"$A\",\"$B\",\"$C\"]}"
  [ "$CODE" = 201 ] || say "warning: creating channel smoke-room gave HTTP $CODE"
  touch "$RUN/seeded"
  say "seeded project $P: alpha, beta (claude-code, model $MODEL), gamma (scripted), channel smoke-room"
}

cmd_steps() {
  live_run
  cat <<EOF

Real server run: $BASE   (run dir $RUN)
Script: $D/real-server-smoke.sh   Full notes: docs/design/REAL-SERVER-RUN.md

Project "$PROJECT_LIVE" holds alpha and beta (Claude Code, model $MODEL: each turn is a real, cheap
model call on your Claude login) and gamma (scripted test agent: no model call).

 1. Session appears. The app shows no demo data. Open project $PROJECT_LIVE: alpha, beta, gamma are
    listed. Also create one from the app (new session, project $PROJECT_LIVE, Claude Code, model
    $MODEL) and confirm it appears in the app and in: $0 status
 2. Transcript streams. In alpha send:
      Count from 1 to 30, one number per line, then say DONE.
    The reply grows while it is written, not all at once at the end.
 3. Approval. In alpha send:
      Run the shell command date and tell me what it printed.
    A Bash approval card appears. Allow once: the tool result and the answer follow. Ask again and
    Deny: the agent reports it was refused. In gamma any message gives an approval card with no
    model call (allow gives "echo: <your text>").
 4. Survives an app restart. Quit the app (or leave it), then run:
      $0 app
    Reopen alpha: the whole transcript is there and its status is right. Optional, the server too:
      $0 server-restart
    then send alpha "What did I ask you first?" (native resume, FR-S2). To verify live server update
    handoff while a turn is streaming (FR-S8, #128): start a long streaming turn in alpha and beta,
    then run:
      $0 server-update
    The script verifies every shim and agent PID stays alive while the server is replaced, then the
    new server reattaches and finishes the turn without losing output.
 5. A2A between two sessions. In alpha's composer send:
      @@beta Reply to alpha with the single word PONG using ember-a2a, then stop.
    beta's transcript gets an "[ember a2a]" message from alpha, beta answers, and alpha receives
    the reply as an "[ember a2a]" message. (Needs ember-a2a next to ember-server.)
 6. Channel post. Open channel smoke-room and post:
      @beta say hello in this channel with channel_post
    The message reaches beta only; beta's reply appears in the channel, attributed to beta. A post
    with no @mention reaches no agent.
 7. Computer. The computers screen shows smoke-node (127.0.0.1:$NODE_PORT) as reachable.

When done:  $0 down     (stops app, server, node and agents; keeps the data)
            $0 reset    (also deletes $RUN)
Logs: $LOGS/{server,node,app}.log
EOF
}

cmd_up() {
  live_run
  resolve_bins app
  mkdirs
  start_server
  start_node
  seed_live
  start_app
  cmd_steps
}

cmd_status() {
  live_run
  local f
  for f in server node app; do
    if alive "$RUN/$f.pid"; then echo "ember-$f: running (pid $(cat "$RUN/$f.pid"))"; else echo "ember-$f: not running"; fi
  done
  alive "$RUN/server.pid" || return 0
  [ -f "$HDR" ] || return 0
  api GET "/api/computers"
  echo "computers:"; j '.[] | "  \(.name)  reachable=\(.reachable)"' 2>/dev/null || true
  api GET "/api/sessions?project=$PROJECT_LIVE"
  echo "sessions in $PROJECT_LIVE:"
  j '.[] | "  \(.title)  \(.agent)  model=\(.model // "-")  status=\(.status)  events=\(.last_seq)  id=\(.id)"' 2>/dev/null || true
  api GET "/api/channels"
  echo "channels:"; j '.[] | "  #\(.name)  project=\(.project)  id=\(.id)"' 2>/dev/null || true
}

cmd_app() { live_run; resolve_bins app >/dev/null; alive "$RUN/server.pid" || die "the server is not running; run: $0 up"; start_app; }

cmd_server_restart() {
  live_run; resolve_bins >/dev/null
  stop_pid "$RUN/server.pid" ember-server
  start_server
  say "agents under session shims kept running; the app reconnects by itself"
}

cmd_server_update() {
  live_run; resolve_bins >/dev/null
  [ -n "${EMBER_SERVER_UPDATE_BIN:-}" ] && SERVER_BIN="$EMBER_SERVER_UPDATE_BIN"
  local f spid apid count=0
  stop_pid "$RUN/server.pid" ember-server
  for f in "$SD"/shims/*.json; do
    [ -f "$f" ] || continue
    spid="$(jq -r '.shim_pid // empty' "$f" 2>/dev/null || true)"
    apid="$(jq -r '.agent_pid // empty' "$f" 2>/dev/null || true)"
    if [ -n "$spid" ] && [ -n "$apid" ] && kill -0 "$spid" 2>/dev/null && kill -0 "$apid" 2>/dev/null; then
      count=$((count + 1))
      say "shim $(basename "$f" .json): shim_pid=$spid and agent_pid=$apid stayed alive while server is down"
    else
      die "shim $(basename "$f" .json) or its agent died during server update (shim_pid=$spid, agent_pid=$apid)"
    fi
  done
  start_server
  say "updated server started ($SERVER_BIN); $count live shim session(s) survived and rejoined"
}

cmd_down() { live_run; [ -d "$RUN" ] || { say "nothing to stop"; return 0; }; stop_all; }

cmd_reset() {
  cmd_down
  [ -d "$RUN" ] && rm -rf "$RUN" && say "deleted $RUN"
  return 0
}

case "${1:-}" in
  check) cmd_check ;;
  up) cmd_up ;;
  steps) cmd_steps ;;
  status) cmd_status ;;
  app) cmd_app ;;
  server-restart) cmd_server_restart ;;
  server-update) cmd_server_update ;;
  down) cmd_down ;;
  reset) cmd_reset ;;
  *) sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
