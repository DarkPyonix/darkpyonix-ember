#!/bin/sh
# Fake `codex app-server` for FR-S8: answers initialize, thread/start (or resume) and turn/start,
# then streams "tick N " agent-message deltas (N in 1..$FAKE_TICKS, default 25, every 0.2 s) and
# completes the turn. Request ids follow the adapter's order: 0, 1, 2.
if [ "$1" = "--version" ]; then echo "codex-cli 9.9.9"; exit 0; fi
read -r l
echo '{"id":0,"result":{"userAgent":"fake"}}'
read -r l
read -r l
echo '{"id":1,"result":{"thread":{"id":"thr-fake"}}}'
read -r l
echo '{"id":2,"result":{"turn":{"id":"turn-1"}}}'
echo '{"method":"turn/started","params":{"threadId":"thr-fake","turn":{"id":"turn-1"}}}'
i=1
while [ "$i" -le "${FAKE_TICKS:-25}" ]; do
  echo '{"method":"item/agentMessage/delta","params":{"threadId":"thr-fake","turnId":"turn-1","itemId":"m1","delta":"tick '$i' "}}'
  i=$((i + 1))
  sleep 0.2
done
echo '{"method":"turn/completed","params":{"threadId":"thr-fake","turn":{"id":"turn-1","status":"completed"}}}'
cat > /dev/null
