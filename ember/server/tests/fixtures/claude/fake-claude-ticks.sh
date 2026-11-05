#!/bin/sh
# Fake `claude` for FR-S8: after the first user message it says "tick N" for N in 1..$FAKE_TICKS
# (default 30), one assistant message every 0.15 s, then ends the turn and waits for stdin to close.
if [ "$1" = "--version" ]; then echo "9.9.9 (Claude Code)"; exit 0; fi
read -r line
echo '{"type":"system","subtype":"init","session_id":"fake-ticks-'$$'"}'
i=1
while [ "$i" -le "${FAKE_TICKS:-30}" ]; do
  echo '{"type":"assistant","message":{"content":[{"type":"text","text":"tick '$i'"}]}}'
  i=$((i + 1))
  sleep 0.15
done
echo '{"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":1,"output_tokens":1}}'
cat > /dev/null
