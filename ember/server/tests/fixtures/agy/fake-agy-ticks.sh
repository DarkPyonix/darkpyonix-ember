#!/bin/sh
case "$1" in
  --version) echo 1.2.16; exit 0 ;;
  -p) if [ "$2" = /hooks ]; then exec "$(dirname "$0")/fake-agy.sh" "$@"; fi ;;
esac
echo '{"event":"init","conversation_id":"fake-conv"}'
i=1
while [ "$i" -le 25 ]; do
  printf '{"event":"step_update","step_update":{"conversation_id":"fake-conv","step_index":1,"state":"ACTIVE","step_type":"agent_response","text_delta":"tick %s "}}\n' "$i"
  sleep 0.2
  i=$((i + 1))
done
echo '{"event":"result","result":{"conversation_id":"fake-conv","status":"SUCCESS","response":"done"}}'
