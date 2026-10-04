#!/bin/sh
# Fake `agy` for adapter plumbing tests (not a recording; the shapes follow the recorded
# turn*.jsonl and hooks_list_1.2.16.json next to this file). Appends its arguments to ./args.log in
# its working directory, answers `--version` ($FAKE_AGY_VERSION, default the pinned 1.2.16) and
# `-p /hooks --output-format json` (from the --add-dir folder's hooks.json), and for a prompt calls
# Ember's PreToolUse hook from the --add-dir folder the way agy does (stdin payload, cwd = the
# .agents folder), appending the hook's answer to ./decisions.log. A prompt containing "slow" then
# waits to be interrupted.
if [ "$1" = "--version" ]; then echo "${FAKE_AGY_VERSION:-1.2.16}"; exit 0; fi
echo "$*" >> args.log
root=""; prompt=""; prev=""
for a in "$@"; do
  case "$prev" in
    --add-dir) root="$a" ;;
    -p) prompt="$a" ;;
  esac
  prev="$a"
done
if [ "$prompt" = "/hooks" ]; then
  # Lists Ember's hook the way agy 1.2.16 does, if hooks.json registers it.
  hooks="$root/.agents/hooks.json"
  flat=$(tr -d ' \n' < "$hooks" 2>/dev/null)
  case "$flat" in
    *'"ember-approvals":{"PreToolUse":'*'"matcher":"*"'*)
      t=$(printf '%s' "$flat" | sed -n 's/.*"timeout":\([0-9]*\).*/\1/p')
      printf '{"conversation_id":"","status":"SUCCESS","response":"","command":{"name":"hooks","data":{"hooks":[{"name":"ember-approvals","enabled":true,"source":"%s","actions":[{"event":"PreToolUse","matcher":"*","type":"command","command":"sh","timeout_seconds":%s}]}]}}}\n' "$hooks" "${t:-30}"
      ;;
    *) echo '{"conversation_id":"","status":"SUCCESS","response":"","command":{"name":"hooks","data":{"hooks":[]}}}' ;;
  esac
  exit 0
fi
c=fake-conv
echo '{"event":"init","conversation_id":"'$c'","init":{"model":"m","cwd":".","tools":["run_command"],"permission_mode":"request-review"}}'
echo '{"event":"step_update","step_update":{"conversation_id":"'$c'","step_index":0,"state":"DONE","step_type":"user_input"}}'
echo '{"event":"step_update","step_update":{"conversation_id":"'$c'","step_index":1,"state":"DONE","step_type":"agent_response","usage":{"input_tokens":3,"output_tokens":4,"thinking_tokens":1,"cache_read_tokens":0,"total_tokens":8}}}'
echo '{"event":"step_update","step_update":{"conversation_id":"'$c'","step_index":2,"state":"ACTIVE","step_type":"tool","tool_name":"run_command","tool_info":{"name":"run_command","parameters":{"CommandLine":"echo hi"}}}}'
here=$(pwd)
answer=$(printf '%s' '{"conversationId":"'$c'","stepIdx":2,"toolCall":{"name":"run_command","args":{"CommandLine":"echo hi"}}}' \
  | (cd "$root/.agents" && sh ./ember-hook.sh))
echo "$answer" >> "$here/decisions.log"
case "$answer" in
  # printf, not echo: dash's echo would turn the JSON escape \n into a newline.
  *'"allow"'*) printf '%s\n' '{"event":"step_update","step_update":{"conversation_id":"'$c'","step_index":2,"state":"DONE","step_type":"tool","tool_name":"run_command","tool_info":{"name":"run_command","parameters":{"CommandLine":"echo hi"},"output":"hi\n"}}}' ;;
  *) echo '{"event":"step_update","step_update":{"conversation_id":"'$c'","step_index":2,"state":"ERROR","step_type":"tool","tool_name":"run_command","tool_info":{"name":"run_command","parameters":{"CommandLine":"echo hi"},"error":{"type":"TOOL_ERROR","message":"tool call denied by pre-tool hook"}}}}' ;;
esac
case "$prompt" in
  *slow*)
    trap 'exit 130' INT
    sleep 30 >/dev/null 2>&1 &
    wait
    ;;
esac
echo '{"event":"result","result":{"conversation_id":"'$c'","status":"SUCCESS","response":"done","num_turns":1,"usage":{"input_tokens":99,"output_tokens":99}}}'
