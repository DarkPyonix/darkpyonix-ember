#!/bin/sh
# Fake `agy` for adapter plumbing tests (not a recording; the shapes follow the recorded
# turn*.jsonl next to this file). Appends its arguments to ./args.log in its working directory,
# answers `--version` and `-p /hooks`, and for a prompt calls Ember's PreToolUse hook from the
# --add-dir folder the way agy does (stdin payload, cwd = the .agents folder), appending the
# hook's answer to ./decisions.log. A prompt containing "slow" then waits to be interrupted.
if [ "$1" = "--version" ]; then echo "9.9.9"; exit 0; fi
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
  # Lists the hook names it loaded.
  sed -n 's/^ *"\(ember-[a-z-]*\)".*/\1/p' "$root/.agents/hooks.json"
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
echo '{"event":"step_update","step_update":{"conversation_id":"'$c'","step_index":2,"state":"DONE","step_type":"tool","tool_name":"run_command","tool_info":{"name":"run_command","parameters":{"CommandLine":"echo hi"}}}}'
case "$prompt" in
  *slow*)
    trap 'exit 130' INT
    sleep 30 >/dev/null 2>&1 &
    wait
    ;;
esac
echo '{"event":"result","result":{"conversation_id":"'$c'","status":"SUCCESS","response":"done","num_turns":1,"usage":{"input_tokens":99,"output_tokens":99}}}'
